-- Track whether the seed locale has been resolved from a supported system or
-- browser preference. The locale itself remains in app_settings so every
-- settings replacement stays atomic.
CREATE TABLE app_locale_initialization (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    initialized INTEGER NOT NULL CHECK (initialized IN (0, 1))
) STRICT;

INSERT INTO app_locale_initialization (singleton, initialized) VALUES (1, 0);
