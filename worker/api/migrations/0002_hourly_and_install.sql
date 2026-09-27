-- Optional hourly data keeps older collectors compatible.
CREATE TABLE IF NOT EXISTS hourly_rows (
  device_id TEXT NOT NULL REFERENCES devices(id),
  date TEXT NOT NULL,
  hour INTEGER NOT NULL CHECK (hour BETWEEN 0 AND 23),
  client TEXT NOT NULL,
  model_id TEXT NOT NULL,
  tokens INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (device_id, date, hour, client, model_id)
);
CREATE INDEX IF NOT EXISTS idx_hourly_rows_date ON hourly_rows(date);

-- Only hashes of short-lived, single-use installation tickets are stored.
CREATE TABLE IF NOT EXISTS install_tickets (
  hash TEXT PRIMARY KEY,
  platform TEXT NOT NULL,
  tag TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_install_tickets_expiry ON install_tickets(expires_at);
