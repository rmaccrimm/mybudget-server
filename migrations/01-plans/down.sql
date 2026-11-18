-- Back to version 0, an empty database that isn't a plan file (SPEC 15.8).
-- `mybudget-server migrate` never goes this far (SPEC 15.9).

DROP TABLE assignments;
DROP TABLE subtransactions;
DROP TABLE transactions;
DROP TABLE categories;
DROP TABLE category_groups;
DROP TABLE payees;
DROP TABLE accounts;
DROP TABLE plan;
