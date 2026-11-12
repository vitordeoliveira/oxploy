PRAGMA foreign_keys = OFF;
-- #[toasty::breakpoint]
CREATE TABLE "_toasty_new_services" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL,
    "app_name" TEXT NOT NULL,
    "description" TEXT,
    "project_id" INTEGER NOT NULL,
    "created_at" BIGINT NOT NULL,
    "github_app_id" INTEGER,
    "repository" TEXT,
    "branch" TEXT,
    "build_path" TEXT,
    "rust_version" TEXT,
    "cargo_bin" TEXT,
    "cargo_features" TEXT,
    "build_kind" TEXT CHECK ("build_kind" IN ('cargo', 'script', 'release', 'dockerfile')),
    "build_script" TEXT,
    "script_binary" TEXT,
    "github_asset" TEXT,
    "github_asset_binary" TEXT,
    "deploys_new_releases" BOOLEAN,
    "after_build" TEXT,
    "environment" TEXT,
    "port" INTEGER,
    "release_id" INTEGER,
    "stopped_at" BIGINT,
    "domain_offered_at" BIGINT,
    "default_domain_id" INTEGER,
    "mcp_path" TEXT,
    "mcp_token" TEXT,
    "kind" TEXT CHECK ("kind" IN ('app', 'external_mcp')),
    "mcp_url" TEXT,
    "dockerfile" TEXT,
    "container_port" INTEGER
);
-- #[toasty::breakpoint]
INSERT INTO "_toasty_new_services" ("id", "name", "app_name", "description", "project_id", "created_at", "github_app_id", "repository", "branch", "build_path", "rust_version", "cargo_bin", "cargo_features", "build_kind", "build_script", "script_binary", "github_asset", "github_asset_binary", "deploys_new_releases", "after_build", "environment", "port", "release_id", "stopped_at", "domain_offered_at", "default_domain_id", "mcp_path", "mcp_token", "kind", "mcp_url") SELECT "id", "name", "app_name", "description", "project_id", "created_at", "github_app_id", "repository", "branch", "build_path", "rust_version", "cargo_bin", "cargo_features", "build_kind", "build_script", "script_binary", "github_asset", "github_asset_binary", "deploys_new_releases", "after_build", "environment", "port", "release_id", "stopped_at", "domain_offered_at", "default_domain_id", "mcp_path", "mcp_token", "kind", "mcp_url" FROM "services";
-- #[toasty::breakpoint]
DROP TABLE "services";
-- #[toasty::breakpoint]
ALTER TABLE "_toasty_new_services" RENAME TO "services";
-- #[toasty::breakpoint]
-- Dropping the old table dropped its indexes too.
CREATE UNIQUE INDEX "index_services_by_app_name" ON "services" ("app_name");
-- #[toasty::breakpoint]
CREATE INDEX "index_services_by_project_id" ON "services" ("project_id");
-- #[toasty::breakpoint]
PRAGMA foreign_keys = ON;
-- #[toasty::breakpoint]
ALTER TABLE "deployments" ADD COLUMN "image" TEXT;
