ALTER TABLE "mcp_tokens" ADD COLUMN "picks_tools" BOOLEAN;
-- #[toasty::breakpoint]
CREATE TABLE "mcp_token_tools" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "token_id" INTEGER NOT NULL,
    "service_id" INTEGER NOT NULL,
    "name" TEXT NOT NULL,
    "picked_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_mcp_token_tools_by_token_id" ON "mcp_token_tools" ("token_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_mcp_token_tools_by_service_id" ON "mcp_token_tools" ("service_id");
