-- Postgres: the same change as apps/server/migrations/20261124022526_assistant_history.sql
-- Ask AI keeps a member's earlier conversations (history): "New chat" archives the current conversation instead of
-- deleting it. Each member has at most one active conversation per workspace (the one the panel shows); the others
-- are archived. `title` is the member's first message (trimmed, at most 80 characters) until they rename it;
-- `message_count` counts the user and assistant messages the panel shows. Conversations are kept for 30 days after
-- their last use (`expires_at`), at most 100 per member and workspace (the server prunes the oldest).
ALTER TABLE assistant_conversations ADD COLUMN title TEXT;
ALTER TABLE assistant_conversations ADD COLUMN message_count BIGINT NOT NULL DEFAULT 0;
ALTER TABLE assistant_conversations ADD COLUMN active BIGINT NOT NULL DEFAULT 0 CHECK (active IN (0, 1));

-- Conversations past their old 6-hour expiry were already gone for their member: they stay gone.
DELETE FROM assistant_conversations WHERE expires_at <= CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT);

-- Title and message count of the conversations that exist, from what the panel shows (the same rule as
-- `store::derive_title`: line breaks and tabs become spaces, then the text is trimmed and cut at 80 characters).
UPDATE assistant_conversations
SET message_count = (
        SELECT COUNT(*) FROM jsonb_array_elements(assistant_conversations.state_json::jsonb -> 'display') AS entry(value)
        WHERE entry.value ->> 'role' IN ('user', 'assistant')
    ),
    title = (
        SELECT CASE
            WHEN length(opening.text) > 80 THEN rtrim(substr(opening.text, 1, 79)) || '…'
            ELSE NULLIF(opening.text, '')
        END
        FROM (
            SELECT btrim(translate(entry.value ->> 'content', E'\r\n\t', '   ')) AS text
            FROM jsonb_array_elements(assistant_conversations.state_json::jsonb -> 'display')
                WITH ORDINALITY AS entry(value, position)
            WHERE entry.value ->> 'role' = 'user' AND jsonb_typeof(entry.value -> 'content') = 'string'
            ORDER BY entry.position
            LIMIT 1
        ) AS opening
    )
WHERE CASE
    WHEN pg_input_is_valid(state_json, 'jsonb') THEN jsonb_typeof(state_json::jsonb -> 'display') = 'array'
    ELSE false
END;

-- A member's newest conversation in a workspace (before this change they had at most one) becomes the active one.
-- An older one (only left by a race of two "New chat"s) is archived as it is: if it still held an approval, the server
-- never shows or applies it (only the active conversation shows approval cards and runs), and Continue retires it.
UPDATE assistant_conversations
SET active = 1
WHERE NOT EXISTS (
    SELECT 1 FROM assistant_conversations AS newer
    WHERE newer.workspace_id = assistant_conversations.workspace_id
      AND newer.user_id = assistant_conversations.user_id
      AND (newer.updated_at > assistant_conversations.updated_at
           OR (newer.updated_at = assistant_conversations.updated_at AND newer.id > assistant_conversations.id))
);

-- The new retention: 30 days after the last use.
UPDATE assistant_conversations SET expires_at = updated_at + 2592000000;

CREATE UNIQUE INDEX assistant_conversations_active ON assistant_conversations (workspace_id, user_id) WHERE active = 1;
