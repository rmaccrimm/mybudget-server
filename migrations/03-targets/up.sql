-- Schema version 3: assignments and targets (SPEC 18.8).

-- Every change to a category's assigned amount for a month (SPEC 18.2), in
-- the order they were made. A month's assigned amount is the sum of its
-- rows. `id` only identifies the row.
CREATE TABLE assignment_history (
    id          TEXT    NOT NULL PRIMARY KEY,
    position    INTEGER NOT NULL UNIQUE,
    category_id TEXT    NOT NULL,
    month       TEXT    NOT NULL,
    -- The date it was made.
    date        TEXT    NOT NULL,
    amount      INTEGER NOT NULL
) STRICT;

-- Each month's assigned amount that isn't 0 becomes one assignment for the
-- whole amount, dated the first day of the month.
INSERT INTO assignment_history (id, position, category_id, month, date, amount)
SELECT lower(hex(randomblob(16))),
       row_number() OVER (ORDER BY month, category_id),
       category_id, month, month || '-01', assigned
FROM assignments
WHERE assigned != 0;

DROP TABLE assignments;

-- A category's targets (SPEC 18.4), in order of `start_month`.
CREATE TABLE category_targets (
    category_id TEXT    NOT NULL,
    position    INTEGER NOT NULL,
    type        TEXT    NOT NULL,
    amount      INTEGER NOT NULL,
    -- NULL for the month's last day.
    by_date     INTEGER,
    start_month TEXT    NOT NULL,
    -- The last month it applies to; NULL if it has no end.
    end_month   TEXT,
    PRIMARY KEY (category_id, position)
) STRICT;
