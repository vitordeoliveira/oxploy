ALTER TABLE "mcp_tokens" ADD COLUMN "organization_id" INTEGER;
-- #[toasty::breakpoint]
CREATE INDEX "index_mcp_tokens_by_organization_id" ON "mcp_tokens" ("organization_id");
