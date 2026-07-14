
DROP INDEX state_transition_id_unique;
ALTER TABLE state DROP COLUMN transition_id;
