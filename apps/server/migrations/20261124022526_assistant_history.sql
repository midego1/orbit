-- SQLite
-- Ask AI keeps a member's earlier conversations (history): "New chat" archives the current conversation instead of
-- deleting it. Each member has at most one active conversation per workspace (the one the panel shows); the others
-- are archived. `title` is the member's first message (trimmed, at most 80 characters) until they rename it;
-- `message_count` counts the user and assistant messages the panel shows. Conversations are kept for 30 days after
-- their last use (`expires_at`), at most 100 per member and workspace (the server prunes the oldest).
ALTER TABLE assistant_conversations ADD COLUMN title TEXT;
ALTER TABLE assistant_conversations ADD COLUMN message_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assistant_conversations ADD COLUMN active INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1));

-- Conversations past their old 6-hour expiry were already gone for their member: they stay gone.
DELETE FROM assistant_conversations WHERE expires_at <= CAST(strftime('%s', 'now') AS INTEGER) * 1000;

-- Title and message count of the conversations that exist, from what the panel shows (the same rule as
-- `store::derive_title`: line breaks and tabs become spaces, then the text is trimmed and cut at 80 characters).
UPDATE assistant_conversations
SET message_count = (
        SELECT COUNT(*) FROM json_each(assistant_conversations.state_json, '$.display') AS entry
        WHERE json_extract(entry.value, '$.role') IN ('user', 'assistant')
    ),
    title = (
        SELECT CASE
            WHEN length(opening.text) > 80 THEN rtrim(substr(opening.text, 1, 79)) || '…'
            ELSE NULLIF(opening.text, '')
        END
        FROM (
            SELECT trim(replace(replace(replace(json_extract(entry.value, '$.content'), char(13), ' '), char(10), ' '),
                char(9), ' ')) AS text
            FROM json_each(assistant_conversations.state_json, '$.display') AS entry
            WHERE json_extract(entry.value, '$.role') = 'user' AND json_type(entry.value, '$.content') = 'text'
            ORDER BY entry.key
            LIMIT 1
        ) AS opening
    )
WHERE CASE WHEN json_valid(state_json) THEN json_type(state_json, '$.display') = 'array' ELSE 0 END;

-- A member's newest conversation in a workspace (before this change they had at most one) becomes the active one.
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
