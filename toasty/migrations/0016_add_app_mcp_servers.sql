ALTER TABLE "services" ADD COLUMN "mcp_path" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "mcp_token" TEXT;
-- #[toasty::breakpoint]
CREATE TABLE "mcp_tools" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "service_id" INTEGER NOT NULL,
    "name" TEXT NOT NULL,
    "enabled_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_mcp_tools_by_service_id" ON "mcp_tools" ("service_id");
