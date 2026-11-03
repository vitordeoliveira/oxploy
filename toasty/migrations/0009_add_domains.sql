CREATE TABLE "domains" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "service_id" INTEGER NOT NULL,
    "host" TEXT NOT NULL,
    "path" TEXT,
    "strip_path" BOOLEAN NOT NULL,
    "https" BOOLEAN NOT NULL,
    "redirect_www" BOOLEAN NOT NULL,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_domains_by_service_id" ON "domains" ("service_id");
