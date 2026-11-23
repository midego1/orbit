//! A workspace's Ask AI settings (bring your own key), the token usage of its model requests,
//! and the per-user request limit.
//!
//! The API key is sealed with the app key (`secret_box::seal`, AAD `ai-key:<workspace id>`), so
//! a sealed key copied to another workspace's row does not open. It is opened only to make a
//! model request and is never returned to a client or logged: the settings page sees whether a
//! key is set and its last four characters.

use orbit_domain::Permission;
use orbit_platform::{Database, Id, Row, TimestampMillis};
use serde::Serialize;
use thiserror::Error;

use crate::audit::{self, AuditOutcome};
use crate::repositories::membership;
use crate::secret_box;

pub(crate) const PROVIDER_ANTHROPIC: &str = "anthropic";
pub(crate) const DEFAULT_MODEL: &str = "claude-sonnet-5-5";
/// The models the settings page offers, best value first.
pub(crate) const OFFERED_MODELS: [&str; 3] =
    ["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5"];
/// Model requests per user and minute, over every server of the database.
pub(crate) const REQUESTS_PER_MINUTE: i64 = 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AiSettings {
    pub provider: String,
    pub model: String,
    pub key: Option<SealedKey>,
    /// `None`: [`DEFAULT_DAILY_TOKEN_CAP`].
    pub daily_token_cap: Option<i64>,
}

/// The tokens a workspace may use per UTC day unless an admin sets another limit (cache reads count a tenth).
pub(crate) const DEFAULT_DAILY_TOKEN_CAP: i64 = 2_000_000;
pub(crate) const MAX_DAILY_TOKEN_CAP: i64 = 1_000_000_000;

/// Whether a model request may go out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Gate {
    Open,
    /// The user's per-minute limit.
    RateLimited,
    /// The workspace's daily token limit.
    DailyCap,
    /// The limits could not be checked: the request does not go out (fail closed).
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SealedKey {
    pub sealed: Vec<u8>,
    pub last4: String,
}

#[derive(Debug, Error)]
pub(crate) enum SettingsError {
    #[error("workspace not found")]
    NotFound,
    #[error("the role cannot manage the workspace's AI settings")]
    Forbidden,
    #[error("the server has no app key to seal the API key with")]
    NoAppKey,
    #[error("the API key could not be sealed or opened")]
    Sealing,
    #[error("unknown model")]
    UnknownModel,
    #[error("the daily token limit is out of range")]
    InvalidCap,
    #[error("with the server's key the daily token limit is at most {0}")]
    AboveServerMax(i64),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct UsageTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ModelUsage {
    pub model: String,
    /// Requests whose prompt was over 100K tokens (Haiku 5.5 bills those at higher rates).
    #[serde(rename = "longPrompt")]
    pub long_prompt: bool,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

fn aad(workspace_id: Id) -> Vec<u8> {
    format!("ai-key:{workspace_id}").into_bytes()
}

pub(crate) fn seal_key(
    app_key: &[u8; 32],
    workspace_id: Id,
    key: &str,
) -> Result<SealedKey, SettingsError> {
    let sealed = secret_box::seal(app_key, &aad(workspace_id), key.as_bytes())
        .map_err(|()| SettingsError::Sealing)?;
    let last4: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(SealedKey { sealed, last4 })
}

pub(crate) fn open_key(
    app_key: &[u8; 32],
    workspace_id: Id,
    key: &SealedKey,
) -> Result<String, SettingsError> {
    let plain = secret_box::open(app_key, &aad(workspace_id), &key.sealed)
        .map_err(|()| SettingsError::Sealing)?;
    String::from_utf8(plain).map_err(|_| SettingsError::Sealing)
}

#[derive(Clone)]
pub(crate) struct SettingsRepository {
    database: Database,
    /// The highest daily limit of a workspace that runs on the server's key (`ORBIT_AI_MAX_DAILY_TOKENS`).
    server_key_max: i64,
}

impl SettingsRepository {
    pub(crate) const fn new(database: Database) -> Self {
        Self {
            database,
            server_key_max: DEFAULT_DAILY_TOKEN_CAP,
        }
    }

    /// `ORBIT_AI_MAX_DAILY_TOKENS`: an admin of a workspace that uses the server's key (`ORBIT_AI_ANTHROPIC_KEY`)
    /// cannot set a higher daily limit.
    #[must_use]
    pub(crate) fn with_server_key_max(mut self, max: i64) -> Self {
        self.server_key_max = max.clamp(1, MAX_DAILY_TOKEN_CAP);
        self
    }

    pub(crate) const fn server_key_max(&self) -> i64 {
        self.server_key_max
    }

    /// The daily limit that applies: the workspace's own setting (or the default), and with the server's key no
    /// more than the server allows.
    pub(crate) fn effective_cap(&self, settings: Option<&AiSettings>) -> i64 {
        let cap = settings
            .and_then(|settings| settings.daily_token_cap)
            .unwrap_or(DEFAULT_DAILY_TOKEN_CAP);
        if settings.is_some_and(|settings| settings.key.is_some()) {
            cap
        } else {
            cap.min(self.server_key_max)
        }
    }

    /// Whether the user may change the settings (owners and admins: `integrations.manage`).
    pub(crate) async fn can_manage(
        &self,
        workspace_id: Id,
        user_id: Id,
    ) -> Result<bool, SettingsError> {
        let actor = membership::actor(self.database.pool(), workspace_id, user_id)
            .await?
            .ok_or(SettingsError::NotFound)?;
        Ok(actor.can(Permission::IntegrationsManage))
    }

    /// The settings a run uses (any member may run Ask AI; the key stays sealed).
    pub(crate) async fn get(&self, workspace_id: Id) -> Result<Option<AiSettings>, SettingsError> {
        let row = sqlx::query(
            "SELECT provider, model, api_key_sealed, api_key_last4, daily_token_cap FROM workspace_ai_settings \
             WHERE workspace_id = $1",
        )
        .bind(workspace_id.to_string())
        .fetch_optional(self.database.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let sealed: Option<Vec<u8>> = row.try_get("api_key_sealed")?;
        let last4: Option<String> = row.try_get("api_key_last4")?;
        Ok(Some(AiSettings {
            provider: row.try_get("provider")?,
            model: row.try_get("model")?,
            key: sealed
                .zip(last4)
                .map(|(sealed, last4)| SealedKey { sealed, last4 }),
            daily_token_cap: row.try_get("daily_token_cap")?,
        }))
    }

    /// Saves the model and, when `key` is `Some`, replaces the key (`Some(None)` removes it).
    pub(crate) async fn save(
        &self,
        workspace_id: Id,
        user_id: Id,
        model: &str,
        key: Option<Option<SealedKey>>,
        daily_token_cap: Option<i64>,
        now: TimestampMillis,
    ) -> Result<(), SettingsError> {
        if !OFFERED_MODELS.contains(&model) {
            return Err(SettingsError::UnknownModel);
        }
        if daily_token_cap.is_some_and(|cap| !(1..=MAX_DAILY_TOKEN_CAP).contains(&cap)) {
            return Err(SettingsError::InvalidCap);
        }
        let mut transaction = self.database.workspace_transaction(workspace_id).await?;
        // a workspace on the server's key cannot raise its limit above what the server allows
        if let Some(cap) = daily_token_cap
            && cap > self.server_key_max
        {
            let own_key = match &key {
                Some(key) => key.is_some(),
                None => sqlx::query_scalar::<_, i64>(
                    "SELECT CAST(COUNT(*) AS BIGINT) FROM workspace_ai_settings WHERE workspace_id = $1 \
                     AND api_key_sealed IS NOT NULL",
                )
                .bind(workspace_id.to_string())
                .fetch_one(&mut *transaction)
                .await?
                    > 0,
            };
            if !own_key {
                return Err(SettingsError::AboveServerMax(self.server_key_max));
            }
        }
        let actor = membership::actor(&mut *transaction, workspace_id, user_id)
            .await?
            .ok_or(SettingsError::NotFound)?;
        if !actor.can(Permission::IntegrationsManage) {
            return Err(SettingsError::Forbidden);
        }
        let workspace = workspace_id.to_string();
        let user = user_id.to_string();
        sqlx::query(
            "INSERT INTO workspace_ai_settings (workspace_id, provider, model, updated_by, updated_at) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (workspace_id) DO UPDATE SET provider = excluded.provider, \
             model = excluded.model, updated_by = excluded.updated_by, updated_at = excluded.updated_at",
        )
        .bind(&workspace)
        .bind(PROVIDER_ANTHROPIC)
        .bind(model)
        .bind(&user)
        .bind(now.as_millis())
        .execute(&mut *transaction)
        .await?;
        let mut metadata = serde_json::json!({"provider": PROVIDER_ANTHROPIC, "model": model});
        if let Some(key) = &key {
            sqlx::query(
                "UPDATE workspace_ai_settings SET api_key_sealed = $1, api_key_last4 = $2 WHERE workspace_id = $3",
            )
            .bind(key.as_ref().map(|key| key.sealed.clone()))
            .bind(key.as_ref().map(|key| key.last4.clone()))
            .bind(&workspace)
            .execute(&mut *transaction)
            .await?;
            metadata["api_key"] =
                serde_json::json!(if key.is_some() { "replaced" } else { "removed" });
        }
        if let Some(cap) = daily_token_cap {
            sqlx::query(
                "UPDATE workspace_ai_settings SET daily_token_cap = $1 WHERE workspace_id = $2",
            )
            .bind(cap)
            .bind(&workspace)
            .execute(&mut *transaction)
            .await?;
            metadata["daily_token_cap"] = serde_json::json!(cap);
        }
        audit::record(
            &mut transaction,
            workspace_id,
            Some(user_id),
            "workspace.ai_settings_updated",
            AuditOutcome::Success,
            "workspace",
            Some(workspace_id),
            "assistant",
            metadata,
            now,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Stores one model request's usage (a reservation before the request, settled afterwards); returns its id.
    pub(crate) async fn record_usage(
        &self,
        workspace_id: Id,
        user_id: Id,
        conversation_id: Id,
        model: &str,
        usage: super::model::Usage,
        now: TimestampMillis,
    ) -> Result<Id, sqlx::Error> {
        let clamp = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        let id = Id::new_v7();
        let mut transaction = self.database.workspace_transaction(workspace_id).await?;
        sqlx::query(
            "INSERT INTO assistant_usage (id, workspace_id, user_id, conversation_id, provider, model, input_tokens, \
             output_tokens, cache_read_tokens, cache_write_tokens, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .bind(user_id.to_string())
        .bind(conversation_id.to_string())
        .bind(PROVIDER_ANTHROPIC)
        .bind(model)
        .bind(clamp(usage.input_tokens))
        .bind(clamp(usage.output_tokens))
        .bind(clamp(usage.cache_read_tokens))
        .bind(clamp(usage.cache_write_tokens))
        .bind(now.as_millis())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(id)
    }

    /// Replaces a reservation (a row made by [`Self::record_usage`] before the request) with what the request used.
    pub(crate) async fn settle_usage(
        &self,
        workspace_id: Id,
        id: Id,
        model: &str,
        usage: super::model::Usage,
    ) -> Result<(), sqlx::Error> {
        let clamp = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        let mut transaction = self.database.workspace_transaction(workspace_id).await?;
        sqlx::query(
            "UPDATE assistant_usage SET model = $1, input_tokens = $2, output_tokens = $3, cache_read_tokens = $4, \
             cache_write_tokens = $5 WHERE id = $6 AND workspace_id = $7",
        )
        .bind(model)
        .bind(clamp(usage.input_tokens))
        .bind(clamp(usage.output_tokens))
        .bind(clamp(usage.cache_read_tokens))
        .bind(clamp(usage.cache_write_tokens))
        .bind(id.to_string())
        .bind(workspace_id.to_string())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    }

    /// Token usage per model since `since`.
    pub(crate) async fn usage_since(
        &self,
        workspace_id: Id,
        since: TimestampMillis,
    ) -> Result<Vec<ModelUsage>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT model, CAST(CASE WHEN input_tokens + cache_read_tokens + cache_write_tokens > $3 THEN 1 ELSE 0 END \
             AS BIGINT) AS long_prompt, CAST(COUNT(*) AS BIGINT) AS requests, CAST(SUM(input_tokens) AS BIGINT) AS input_tokens, \
             CAST(SUM(output_tokens) AS BIGINT) AS output_tokens, CAST(SUM(cache_read_tokens) AS BIGINT) AS cache_read_tokens, \
             CAST(SUM(cache_write_tokens) AS BIGINT) AS cache_write_tokens FROM assistant_usage \
             WHERE workspace_id = $1 AND created_at >= $2 GROUP BY model, long_prompt ORDER BY model, long_prompt",
        )
        .bind(workspace_id.to_string())
        .bind(since.as_millis())
        .bind(HAIKU_LONG_PROMPT)
        .fetch_all(self.database.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(ModelUsage {
                    model: row.try_get("model")?,
                    long_prompt: row.try_get::<i64, _>("long_prompt")? == 1,
                    totals: UsageTotals {
                        requests: row.try_get("requests")?,
                        input_tokens: row.try_get::<Option<i64>, _>("input_tokens")?.unwrap_or(0),
                        output_tokens: row.try_get::<Option<i64>, _>("output_tokens")?.unwrap_or(0),
                        cache_read_tokens: row
                            .try_get::<Option<i64>, _>("cache_read_tokens")?
                            .unwrap_or(0),
                        cache_write_tokens: row
                            .try_get::<Option<i64>, _>("cache_write_tokens")?
                            .unwrap_or(0),
                    },
                })
            })
            .collect()
    }

    /// The workspace's tokens of this UTC day, as the daily limit counts them: cache reads count a tenth.
    pub(crate) async fn tokens_today(
        &self,
        workspace_id: Id,
        now: TimestampMillis,
    ) -> Result<i64, sqlx::Error> {
        let day_start = now.as_millis() - now.as_millis().rem_euclid(86_400_000);
        let row = sqlx::query(
            "SELECT CAST(SUM(input_tokens + output_tokens + cache_write_tokens) AS BIGINT) AS full_tokens, \
             CAST(SUM(cache_read_tokens) AS BIGINT) AS read_tokens FROM assistant_usage \
             WHERE workspace_id = $1 AND created_at >= $2",
        )
        .bind(workspace_id.to_string())
        .bind(day_start)
        .fetch_one(self.database.pool())
        .await?;
        let full: Option<i64> = row.try_get("full_tokens")?;
        let read: Option<i64> = row.try_get("read_tokens")?;
        Ok(full.unwrap_or(0).saturating_add(read.unwrap_or(0) / 10))
    }

    /// Whether `user_id` may make one more model request now: the workspace's daily token limit, then the user's
    /// per-minute limit (which this counts). Any database error closes the gate.
    pub(crate) async fn gate(&self, workspace_id: Id, user_id: Id, now: TimestampMillis) -> Gate {
        let checked = async {
            let cap = self.effective_cap(self.get(workspace_id).await.map_err(|_| ())?.as_ref());
            if self.tokens_today(workspace_id, now).await.map_err(|_| ())? >= cap {
                return Ok(Gate::DailyCap);
            }
            Ok::<_, ()>(if self.take_request(user_id, now).await.map_err(|_| ())? {
                Gate::Open
            } else {
                Gate::RateLimited
            })
        }
        .await;
        checked.unwrap_or_else(|()| {
            tracing::warn!("assistant limits could not be checked; the request is refused");
            Gate::Unavailable
        })
    }

    /// Counts one model request of `user_id` in this minute's window (`rate_limit_windows`, shared
    /// by every server); `false` when the user is over the limit.
    pub(crate) async fn take_request(
        &self,
        user_id: Id,
        now: TimestampMillis,
    ) -> Result<bool, sqlx::Error> {
        let minute = now.as_millis().div_euclid(60_000);
        let hits: i64 = sqlx::query_scalar(
            "INSERT INTO rate_limit_windows (bucket, window_minute, hits) VALUES ($1, $2, 1) \
             ON CONFLICT (bucket, window_minute) DO UPDATE SET hits = rate_limit_windows.hits + 1 \
             RETURNING hits",
        )
        .bind(format!("assistant:user:{user_id}"))
        .bind(minute)
        .fetch_one(self.database.pool())
        .await?;
        Ok(hits <= REQUESTS_PER_MINUTE)
    }
}

/// Anthropic's list prices in US dollars per million tokens (Claude API), as the `claude-api` reference gives them
/// (checked November 2026). `input` is uncached input; Anthropic reports cache reads and cache writes separately
/// (`input_tokens` excludes both), so each kind is priced on its own. Thinking is billed as output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// 5-minute cache writes (the only kind Ask AI makes): 1.25 times input.
    pub cache_write: f64,
}

/// A prompt longer than this (input + cache reads + cache writes) is billed at Haiku 5.5's long-prompt rates.
pub(crate) const HAIKU_LONG_PROMPT: i64 = 100_000;

/// The rates of a model (`long_prompt`: the request's prompt was over 100K tokens), or `None` for a model without
/// known prices.
pub(crate) fn rates(model: &str, long_prompt: bool) -> Option<Rates> {
    let (input, output, cache_read) = match model {
        m if m.starts_with("claude-sonnet-5-5") => (2.0, 10.0, 0.10),
        m if m.starts_with("claude-opus-5-5") => (4.0, 20.0, 0.20),
        m if m.starts_with("claude-haiku-5-5") && long_prompt => (0.50, 2.50, 0.05),
        m if m.starts_with("claude-haiku-5-5") => (0.10, 0.50, 0.01),
        // models a refusal fallback can answer with
        m if m.starts_with("claude-sonnet-5") => (2.0, 10.0, 0.20),
        m if m.starts_with("claude-opus-5") || m.starts_with("claude-opus-4-8") => {
            (5.0, 25.0, 0.50)
        }
        _ => return None,
    };
    Some(Rates {
        input,
        output,
        cache_read,
        cache_write: input * 1.25,
    })
}

/// The estimated cost in US dollars of a model's usage at list prices, or `None` without known prices.
pub(crate) fn estimated_cost(model: &str, long_prompt: bool, totals: &UsageTotals) -> Option<f64> {
    let rates = rates(model, long_prompt)?;
    #[allow(clippy::cast_precision_loss)]
    let million = |tokens: i64| tokens as f64 / 1_000_000.0;
    Some(
        million(totals.input_tokens) * rates.input
            + million(totals.output_tokens) * rates.output
            + million(totals.cache_read_tokens) * rates.cache_read
            + million(totals.cache_write_tokens) * rates.cache_write,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn a_sealed_key_opens_only_for_its_workspace() {
        let workspace = Id::new_v7();
        let sealed = seal_key(&KEY, workspace, "sk-ant-test-0123456789abcd").unwrap();
        assert_eq!(sealed.last4, "abcd");
        assert!(!sealed.sealed.windows(4).any(|window| window == b"abcd"));
        assert_eq!(
            open_key(&KEY, workspace, &sealed).unwrap(),
            "sk-ant-test-0123456789abcd"
        );
        assert!(open_key(&KEY, Id::new_v7(), &sealed).is_err());
        assert!(open_key(&[8; 32], workspace, &sealed).is_err());
    }

    #[test]
    fn the_settings_never_print_the_key() {
        let sealed = seal_key(&KEY, Id::new_v7(), "sk-ant-secret-wxyz").unwrap();
        let shown = format!("{sealed:?}");
        assert!(!shown.contains("secret"));
    }

    #[test]
    fn costs_are_estimated_for_the_offered_models() {
        let totals = UsageTotals {
            requests: 1,
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            ..UsageTotals::default()
        };
        let cost = estimated_cost("claude-sonnet-5-5", false, &totals).unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
        assert_eq!(estimated_cost("other", false, &totals), None);
        // each kind of input at its own rate: 1M cache reads cost $0.10, 1M cache writes $2.50 on Sonnet 5.5
        let cached = UsageTotals {
            requests: 1,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 1_000_000,
            ..UsageTotals::default()
        };
        assert!((estimated_cost("claude-sonnet-5-5", false, &cached).unwrap() - 2.60).abs() < 1e-9);
        assert!((estimated_cost("claude-opus-5-5", false, &cached).unwrap() - 5.20).abs() < 1e-9);
        // Haiku 5.5 bills prompts over 100K tokens at its long-prompt rates
        assert!((estimated_cost("claude-haiku-5-5", false, &totals).unwrap() - 0.15).abs() < 1e-9);
        assert!((estimated_cost("claude-haiku-5-5", true, &totals).unwrap() - 0.75).abs() < 1e-9);
    }
}
