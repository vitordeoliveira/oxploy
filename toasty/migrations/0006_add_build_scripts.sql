ALTER TABLE "services" ADD COLUMN "script_binary" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "build_kind" TEXT CHECK ("build_kind" IN ('cargo', 'script'));
-- #[toasty::breakpoint]
ALTER TABLE "services" ADD COLUMN "build_script" TEXT;
