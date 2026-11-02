ALTER TABLE "services" ADD COLUMN "cargo_features" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "repository" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "github_app_id" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "build_path" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "branch" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "cargo_bin" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "rust_version" TEXT;
-- #[toasty::breakpoint]
CREATE TABLE "github_apps" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "organization_id" INTEGER NOT NULL,
    "app_id" INTEGER NOT NULL,
    "name" TEXT NOT NULL,
    "html_url" TEXT NOT NULL,
    "private_key" TEXT NOT NULL,
    "client_id" TEXT NOT NULL,
    "client_secret" TEXT NOT NULL,
    "webhook_secret" TEXT,
    "installation_id" INTEGER,
    "account" TEXT,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_github_apps_by_organization_id" ON "github_apps" ("organization_id");
