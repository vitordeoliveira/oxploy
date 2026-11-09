ALTER TABLE "services" ADD COLUMN "mcp_url" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "kind" TEXT CHECK ("kind" IN ('app', 'external_mcp'));
