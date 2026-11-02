ALTER TABLE "users" ADD COLUMN "default_organization_id" INTEGER;
-- #[toasty::breakpoint]
CREATE TABLE "user_sessions" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "token_hash" BLOB NOT NULL,
    "user_id" INTEGER NOT NULL,
    "expires_at" BIGINT NOT NULL,
    "active_organization_id" INTEGER
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_user_sessions_by_token_hash" ON "user_sessions" ("token_hash");
-- #[toasty::breakpoint]
CREATE INDEX "index_user_sessions_by_user_id" ON "user_sessions" ("user_id");
-- #[toasty::breakpoint]
CREATE TABLE "organizations" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL,
    "logo_url" TEXT,
    "owner_id" INTEGER NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_organizations_by_owner_id" ON "organizations" ("owner_id");
