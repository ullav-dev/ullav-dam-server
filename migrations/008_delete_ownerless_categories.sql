-- Categories with no owner_id are invalid and cannot be privacy-filtered.
-- Remove them so only properly-owned categories remain in the database.
DELETE FROM categories WHERE owner_id = '';
