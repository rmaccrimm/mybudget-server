-- Schema version 1: everything sections 1 to 13 define (SPEC 15.8).
--
-- A plan file holds one plan. Each kind of object has a table of its own,
-- with a column for each field the API shows. A list's order is kept in a
-- `position` column: rows listed by ascending `position` are in the list's
-- order, and deleted rows leave gaps.

-- The plan itself, in one row.
CREATE TABLE plan (
    id            TEXT    NOT NULL PRIMARY KEY,
    name          TEXT    NOT NULL,
    currency_code TEXT    NOT NULL,
    -- Microseconds since the Unix epoch. Plans are listed oldest first
    -- (SPEC 4.2).
    created_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE accounts (
    id                  TEXT    NOT NULL PRIMARY KEY,
    position            INTEGER NOT NULL UNIQUE,
    name                TEXT    NOT NULL,
    type                TEXT    NOT NULL,
    closed              INTEGER NOT NULL,
    note                TEXT,
    transfer_payee_id   TEXT    NOT NULL,
    payment_category_id TEXT
) STRICT;

-- Transfer payees included.
CREATE TABLE payees (
    id                  TEXT    NOT NULL PRIMARY KEY,
    position            INTEGER NOT NULL UNIQUE,
    name                TEXT    NOT NULL,
    transfer_account_id TEXT
) STRICT;

-- The internal Credit Card Payments group comes first.
CREATE TABLE category_groups (
    id       TEXT    NOT NULL PRIMARY KEY,
    position INTEGER NOT NULL UNIQUE,
    name     TEXT    NOT NULL,
    hidden   INTEGER NOT NULL,
    internal INTEGER NOT NULL
) STRICT;

-- Within a group, categories are in the order they joined it (SPEC 7.1).
CREATE TABLE categories (
    id                 TEXT    NOT NULL PRIMARY KEY,
    position           INTEGER NOT NULL UNIQUE,
    group_id           TEXT    NOT NULL,
    name               TEXT    NOT NULL,
    hidden             INTEGER NOT NULL,
    note               TEXT,
    payment_account_id TEXT
) STRICT;

-- `position` is the order transactions were created in, which orders those
-- on the same date (SPEC 8.5).
CREATE TABLE transactions (
    id                      TEXT    NOT NULL PRIMARY KEY,
    position                INTEGER NOT NULL UNIQUE,
    account_id              TEXT    NOT NULL,
    date                    TEXT    NOT NULL,
    amount                  INTEGER NOT NULL,
    payee_id                TEXT,
    -- A category's id or `ready_to_assign`; NULL on a split and on a
    -- transfer half without a category.
    category_id             TEXT,
    memo                    TEXT,
    cleared                 TEXT    NOT NULL,
    approved                INTEGER NOT NULL,
    flag_color              TEXT,
    transfer_account_id     TEXT,
    transfer_transaction_id TEXT
) STRICT;

-- A split's lines, in the order they were given (SPEC 10.3).
CREATE TABLE subtransactions (
    transaction_id TEXT    NOT NULL,
    position       INTEGER NOT NULL,
    amount         INTEGER NOT NULL,
    category_id    TEXT    NOT NULL,
    memo           TEXT,
    PRIMARY KEY (transaction_id, position)
) STRICT;

-- Assigned amounts by category and month (SPEC 11.2). A month without a row
-- has 0 assigned.
CREATE TABLE assignments (
    category_id TEXT    NOT NULL,
    month       TEXT    NOT NULL,
    assigned    INTEGER NOT NULL,
    PRIMARY KEY (category_id, month)
) STRICT;
