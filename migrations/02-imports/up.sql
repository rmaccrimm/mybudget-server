-- Schema version 2: what imports keep (SPEC 17.15).

-- A transaction's `import` (SPEC 17.9); a transaction without a row has
-- `"import": null`.
CREATE TABLE transaction_imports (
    transaction_id TEXT    NOT NULL PRIMARY KEY,
    import_id      TEXT    NOT NULL,
    date           TEXT    NOT NULL,
    amount         INTEGER NOT NULL,
    payee_name     TEXT,
    memo           TEXT,
    matched        INTEGER NOT NULL
) STRICT;

-- The import ids each account has had (SPEC 17.9).
CREATE TABLE account_import_ids (
    account_id TEXT NOT NULL,
    import_id  TEXT NOT NULL,
    PRIMARY KEY (account_id, import_id)
) STRICT;

-- What the last import of each statement used (SPEC 17.13), under the
-- statement's type, bank id (NULL when it has none) and account number.
CREATE TABLE import_options (
    type            TEXT    NOT NULL,
    bank_id         TEXT,
    account_number  TEXT    NOT NULL,
    account_id      TEXT    NOT NULL,
    swap_payee_memo INTEGER NOT NULL,
    invert_amounts  INTEGER NOT NULL
) STRICT;

-- A bank id is never empty (SPEC 17.3), so '' can stand for NULL here.
CREATE UNIQUE INDEX import_options_by_statement
    ON import_options (type, IFNULL(bank_id, ''), account_number);
