-- How well the session went, 1 (poor) to 10 (excellent): the owner's optional score when they archive it
-- (`archive [done|partial|not-done] [1-10] [note]`), beside `outcome`. NULL when unsaid or still open.
ALTER TABLE sessions ADD COLUMN rating INTEGER CHECK (rating IS NULL OR (rating BETWEEN 1 AND 10));
