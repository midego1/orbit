-- Postgres: the same change as apps/server/migrations/20261124060850_assistant_chat_models.sql
-- Ask AI: the models members may pick per chat (a JSON array of model ids; NULL: only the workspace's model).
-- A chat's own pick lives in its conversation state; every run checks it against this list again.
ALTER TABLE workspace_ai_settings ADD COLUMN allowed_models TEXT;
-- The model this chat uses (NULL: the workspace's model). The server sets it only to a model of the list above.
ALTER TABLE assistant_conversations ADD COLUMN model TEXT;
