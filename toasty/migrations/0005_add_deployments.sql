ALTER TABLE "services" ADD COLUMN "port" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "release_id" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "stopped_at" BIGINT;
-- #[toasty::breakpoint]
CREATE TABLE "deployments" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "service_id" INTEGER NOT NULL,
    "status" TEXT NOT NULL CHECK ("status" IN ('queued', 'building', 'starting', 'succeeded', 'failed')),
    "commit" TEXT,
    "commit_message" TEXT,
    "binary" TEXT,
    "build_path" TEXT,
    "created_at" BIGINT NOT NULL,
    "finished_at" BIGINT
);
-- #[toasty::breakpoint]
CREATE INDEX "index_deployments_by_service_id" ON "deployments" ("service_id");
