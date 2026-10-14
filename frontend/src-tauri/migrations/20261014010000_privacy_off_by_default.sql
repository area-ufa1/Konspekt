-- Hiding names before a model off this machine is now off until the owner turns
-- it on: for lectures it hides the subject's own name and keeps scans from being
-- read. This also switches it off on installs that already have the row (the
-- two earlier migrations create the row with 1; this one runs after them, so a
-- fresh database ends up off as well). The switch in the window stays.
UPDATE privacy_settings SET anonymize_cloud = 0 WHERE id = '1';
