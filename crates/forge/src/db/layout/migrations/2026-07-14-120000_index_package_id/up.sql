
-- Selected-package materialization must never scan unrelated layout rows.
CREATE INDEX layout_package_id_idx ON layout(package_id);
