-- Learned only from an authenticated collector upload, never console proxies.
CREATE TABLE IF NOT EXISTS api_origin (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  origin TEXT NOT NULL
);
