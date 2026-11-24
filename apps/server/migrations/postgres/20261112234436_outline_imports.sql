-- Postgres: the same change as apps/server/migrations/20261112234436_outline_imports.sql
-- Outline imports: one row per import a workspace admin started, from an uploaded Outline export
-- (`source = 'archive'`: the JSON or the Markdown workspace export as a .zip, stored like an
-- Obsidian vault until a final state) or from Outline's API (`source = 'api'`: base URL and an
-- API token, kept only as ciphertext under the app key and wiped (NULL) in every final state).
-- `format` is what the scan found: `json` or `markdown` for an archive, `api` for the API.
-- An import belongs to its creator; its destination is recorded at start (as for Obsidian).
CREATE TABLE outline_imports (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('archive', 'api')),
    status TEXT NOT NULL CHECK (status IN (
        'scanning', 'ready', 'queued', 'importing', 'completed', 'failed', 'cancelled', 'expired'
    )),
    format TEXT CHECK (format IN ('json', 'markdown', 'api')),
    file_name TEXT NOT NULL DEFAULT '',
    archive_bytes BIGINT NOT NULL DEFAULT 0 CHECK (archive_bytes >= 0),
    archive_location TEXT CHECK (archive_location IN ('local', 'bucket', 'workspace_bucket')),
    base_url TEXT,
    token_ciphertext BYTEA,
    tree_json TEXT,
    selection_json TEXT,
    target_teamspace_id TEXT REFERENCES teamspaces(id) ON DELETE SET NULL,
    target_private BIGINT NOT NULL DEFAULT 0 CHECK (target_private IN (0, 1)),
    target_parent_page_id TEXT REFERENCES pages(id) ON DELETE SET NULL,
    total BIGINT NOT NULL DEFAULT 0 CHECK (total >= 0),
    done BIGINT NOT NULL DEFAULT 0 CHECK (done >= 0),
    failed BIGINT NOT NULL DEFAULT 0 CHECK (failed >= 0),
    skipped BIGINT NOT NULL DEFAULT 0 CHECK (skipped >= 0),
    attachments_total BIGINT NOT NULL DEFAULT 0 CHECK (attachments_total >= 0),
    attachments_done BIGINT NOT NULL DEFAULT 0 CHECK (attachments_done >= 0),
    report_json TEXT,
    error TEXT,
    started_at BIGINT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    CHECK (target_private = 0 OR target_teamspace_id IS NULL),
    CHECK (token_ciphertext IS NULL OR status IN ('scanning', 'ready', 'queued', 'importing'))
);

CREATE INDEX outline_imports_user ON outline_imports (user_id, workspace_id, created_at);
CREATE INDEX outline_imports_workspace ON outline_imports (workspace_id);
CREATE INDEX outline_imports_status ON outline_imports (status, updated_at);
CREATE INDEX outline_imports_target_teamspace ON outline_imports (target_teamspace_id);
CREATE INDEX outline_imports_target_parent ON outline_imports (target_parent_page_id);

-- The destination must belong to the import's workspace.
CREATE FUNCTION outline_imports_target_scope_insert_fn() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.target_teamspace_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM teamspaces
        WHERE teamspaces.id = NEW.target_teamspace_id AND teamspaces.workspace_id = NEW.workspace_id
    )) OR (NEW.target_parent_page_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM pages
        WHERE pages.id = NEW.target_parent_page_id AND pages.workspace_id = NEW.workspace_id
    ))
    THEN
        RAISE EXCEPTION 'outline import target scope mismatch';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER outline_imports_target_scope_insert
    BEFORE INSERT ON outline_imports
    FOR EACH ROW
    EXECUTE FUNCTION outline_imports_target_scope_insert_fn();
CREATE FUNCTION outline_imports_target_scope_update_fn() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.workspace_id <> OLD.workspace_id
        OR (NEW.target_teamspace_id IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM teamspaces
            WHERE teamspaces.id = NEW.target_teamspace_id AND teamspaces.workspace_id = NEW.workspace_id
        ))
        OR (NEW.target_parent_page_id IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM pages
            WHERE pages.id = NEW.target_parent_page_id AND pages.workspace_id = NEW.workspace_id
        ))
    THEN
        RAISE EXCEPTION 'outline import target scope mismatch';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER outline_imports_target_scope_update
    BEFORE UPDATE OF workspace_id, target_teamspace_id, target_parent_page_id ON outline_imports
    FOR EACH ROW
    EXECUTE FUNCTION outline_imports_target_scope_update_fn();

-- The import plan: one row per selected collection, document, template group or template, in
-- creation order (`position`: parents before children). `item_key` is the scan tree node's key
-- (the node keeps the Outline ids and metadata in `outline_imports.tree_json`). `page_id` is
-- chosen before the page is created, so a restarted import finds it; `reused` = 1 when the page
-- already existed from an earlier import (`page_external_refs`): such a page is updated or
-- skipped, never trashed.
-- `status`: pending -> created (page exists, no content yet) -> done | skipped | failed.
CREATE TABLE outline_import_items (
    import_id TEXT NOT NULL REFERENCES outline_imports(id) ON DELETE CASCADE,
    item_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('collection', 'document', 'templates', 'template')),
    parent_key TEXT,
    position BIGINT NOT NULL CHECK (position >= 0),
    title TEXT NOT NULL DEFAULT '',
    page_id TEXT,
    reused BIGINT NOT NULL DEFAULT 0 CHECK (reused IN (0, 1)),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'created', 'done', 'skipped', 'failed')),
    error TEXT,
    PRIMARY KEY (import_id, item_key),
    UNIQUE (import_id, position)
);

CREATE INDEX outline_import_items_page ON outline_import_items (page_id);

-- What the import could not carry over exactly (an unsupported block, a failed attachment, an
-- unresolved link or mention), for the downloadable warning list. At most 5,000 per import; a
-- page filled again after a restart replaces its own warnings.
CREATE TABLE outline_import_warnings (
    import_id TEXT NOT NULL REFERENCES outline_imports(id) ON DELETE CASCADE,
    position BIGINT NOT NULL CHECK (position >= 0),
    item_key TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (import_id, position)
);

CREATE INDEX outline_import_warnings_item ON outline_import_warnings (import_id, item_key);

-- Where a page came from in another tool (`external_source`, `external_id`), so importing the
-- same source again updates or skips the page instead of making a duplicate.
-- `source_updated_at` is the source's last edit time when the page was last written.
CREATE TABLE page_external_refs (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    external_source TEXT NOT NULL CHECK (external_source IN ('outline')),
    external_id TEXT NOT NULL CHECK (length(external_id) BETWEEN 1 AND 512),
    page_id TEXT NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    source_updated_at TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (workspace_id, external_source, external_id)
);

CREATE INDEX page_external_refs_page ON page_external_refs (page_id);
