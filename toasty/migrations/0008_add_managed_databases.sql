CREATE TABLE "managed_databases" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "service_id" INTEGER NOT NULL,
    "file" TEXT NOT NULL,
    "variable" TEXT,
    "format" TEXT NOT NULL CHECK ("format" IN ('sqlite_url', 'plain_path')),
    "link" TEXT,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_managed_databases_by_service_id" ON "managed_databases" ("service_id");
