-- SQLite
-- Ask AI: a workspace's model settings with its own API key (bring your own key), the assistant's conversations
-- (kept for a few hours, one per user and workspace at a time) and the token usage of each model request.

-- The key is sealed with the app key (AAD `ai-key:<workspace id>`) and never leaves the server; `api_key_last4` is
-- what the settings page shows.
CREATE TABLE workspace_ai_settings (
    workspace_id TEXT PRIMARY KEY NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK (provider IN ('anthropic')),
    model TEXT NOT NULL,
    api_key_sealed BLOB,
    api_key_last4 TEXT,
    -- The most tokens the workspace may use per UTC day (NULL: the default, 2,000,000). Cache reads count for a
    -- tenth, as they cost a tenth.
    daily_token_cap INTEGER CHECK (daily_token_cap IS NULL OR daily_token_cap > 0),
    updated_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    updated_at INTEGER NOT NULL,
    CHECK ((api_key_sealed IS NULL) = (api_key_last4 IS NULL))
);

-- `state_json` holds the transcript the model sees, the approval waiting for the user and the tools the user allowed
-- for this conversation. `run_id`/`run_until` lease the one run a conversation may have at a time.
CREATE TABLE assistant_conversations (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    state_json TEXT NOT NULL,
    run_id TEXT,
    run_until INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX assistant_conversations_owner ON assistant_conversations (workspace_id, user_id, updated_at);
CREATE INDEX assistant_conversations_expiry ON assistant_conversations (expires_at);

-- One row per model request.
CREATE TABLE assistant_usage (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    conversation_id TEXT,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX assistant_usage_workspace ON assistant_usage (workspace_id, created_at);
