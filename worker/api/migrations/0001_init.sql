-- tokscale-serverless D1 schema.
-- Apply locally:  npx wrangler d1 migrations apply DB --local
-- Apply remotely: npx wrangler d1 migrations apply DB --remote

CREATE TABLE IF NOT EXISTS devices (
  id          TEXT PRIMARY KEY,
  name        TEXT,
  hostname    TEXT,
  os          TEXT,
  arch        TEXT,
  first_seen  TEXT NOT NULL DEFAULT (datetime('now')),
  last_seen   TEXT NOT NULL DEFAULT (datetime('now'))
);

-- One row per device × day × (client, model). Re-submits from the same
-- device overwrite in place, so local clients can resend full exports
-- without coordination.
CREATE TABLE IF NOT EXISTS daily_rows (
  device_id         TEXT NOT NULL REFERENCES devices(id),
  date              TEXT NOT NULL,
  client            TEXT NOT NULL,
  model_id          TEXT NOT NULL,
  provider_id       TEXT,
  input             INTEGER NOT NULL DEFAULT 0,
  output            INTEGER NOT NULL DEFAULT 0,
  cache_read        INTEGER NOT NULL DEFAULT 0,
  cache_write       INTEGER NOT NULL DEFAULT 0,
  reasoning         INTEGER NOT NULL DEFAULT 0,
  cost              REAL NOT NULL DEFAULT 0,
  messages          INTEGER NOT NULL DEFAULT 0,
  cost_is_complete  INTEGER,
  updated_at        TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (device_id, date, client, model_id)
);

-- Qoder plan credits are day-level (not per model) and are not USD, so they
-- stay out of daily_rows.cost.
CREATE TABLE IF NOT EXISTS daily_credits (
  device_id   TEXT NOT NULL REFERENCES devices(id),
  date        TEXT NOT NULL,
  credits     REAL NOT NULL,
  updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (device_id, date)
);

CREATE INDEX IF NOT EXISTS idx_daily_rows_date ON daily_rows(date);
CREATE INDEX IF NOT EXISTS idx_daily_credits_date ON daily_credits(date);
