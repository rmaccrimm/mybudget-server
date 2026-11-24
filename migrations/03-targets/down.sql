-- Back to version 2, which holds only each month's total assigned amount and
-- no targets (SPEC 18.8).

CREATE TABLE assignments (
    category_id TEXT    NOT NULL,
    month       TEXT    NOT NULL,
    assigned    INTEGER NOT NULL,
    PRIMARY KEY (category_id, month)
) STRICT;

-- A month without a row has 0 assigned.
INSERT INTO assignments (category_id, month, assigned)
SELECT category_id, month, sum(amount)
FROM assignment_history
GROUP BY category_id, month
HAVING sum(amount) != 0;

DROP TABLE assignment_history;
DROP TABLE category_targets;
