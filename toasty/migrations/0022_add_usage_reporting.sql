CREATE TABLE "telemetries" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "install_id" TEXT NOT NULL,
    "reports" BOOLEAN NOT NULL,
    "shares_usage" BOOLEAN NOT NULL,
    "sent_at" BIGINT,
    "failed_at" BIGINT,
    "error" TEXT,
    "created_at" BIGINT NOT NULL
);
