CREATE TABLE "mcp_tokens" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL,
    "token_hash" BLOB NOT NULL,
    "created_at" BIGINT NOT NULL,
    "last_used_at" BIGINT
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_mcp_tokens_by_token_hash" ON "mcp_tokens" ("token_hash");
