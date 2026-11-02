CREATE TABLE "services" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL,
    "app_name" TEXT NOT NULL,
    "description" TEXT,
    "project_id" INTEGER NOT NULL,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_services_by_app_name" ON "services" ("app_name");
-- #[toasty::breakpoint]
CREATE INDEX "index_services_by_project_id" ON "services" ("project_id");
-- #[toasty::breakpoint]
CREATE TABLE "projects" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL,
    "description" TEXT,
    "organization_id" INTEGER NOT NULL,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_projects_by_organization_id" ON "projects" ("organization_id");
