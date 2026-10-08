-- Undoes 0065_accounts, from the referencing table to the referenced one.
ALTER TABLE brain_handler_failures DROP COLUMN IF EXISTS account;
DROP TABLE IF EXISTS brain_pairing_codes;
DROP TABLE IF EXISTS brain_account_credentials;
DROP TABLE IF EXISTS brain_accounts;
DROP TABLE IF EXISTS brain_host_identity;
