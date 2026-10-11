-- The search query as it was before each arm filtered `search_docs` itself (a `docs` CTE, scanned
-- in full), with the same tie-breaks as search.rs. The search-index e2e checks that search.rs ranks
-- a fixture the same. psql variables: q (the query), vec (its embedding, '' for none), exact (the
-- exact candidate, '' for none), kinds (a text[] literal), lim. The vector arm is exact here
-- (no index), so the scenario's fixture stays small enough for HNSW to find the same neighbours.
WITH q AS (SELECT websearch_to_tsquery('simple', :'q') AS tsq),
docs AS MATERIALIZED (SELECT * FROM search_docs WHERE kind = ANY(:'kinds'::text[])),
exact AS (
  SELECT id, row_number() OVER (ORDER BY (ident ILIKE '%' || NULLIF(:'exact', '') || '%') DESC, similarity(ident, NULLIF(:'exact', '')) DESC, id) AS r
  FROM docs WHERE NULLIF(:'exact', '') IS NOT NULL AND ident ILIKE '%' || replace(replace(NULLIF(:'exact', ''), '%', '\%'), '_', '\_') || '%'
  ORDER BY r LIMIT 10),
fts AS (
  SELECT id, row_number() OVER (ORDER BY ts_rank_cd(tsv, q.tsq, 32) DESC, id) AS r
  FROM docs, q WHERE tsv @@ q.tsq ORDER BY r LIMIT 50),
vec AS (
  SELECT id, row_number() OVER () AS r FROM (
    SELECT id FROM docs WHERE NULLIF(:'vec', '') IS NOT NULL AND embedding IS NOT NULL AND embed_model = 'openai/text-embedding-3-small'
    ORDER BY embedding <=> NULLIF(:'vec', '')::vector LIMIT 50) v)
SELECT string_agg(d.ref, ',' ORDER BY o) FROM (
  SELECT d.ref, row_number() OVER (ORDER BY (e.id IS NOT NULL) DESC, e.r NULLS LAST,
           coalesce(1.0 / (60 + f.r), 0) + coalesce(1.0 / (60 + v.r), 0) DESC, d.id) AS o
  FROM docs d LEFT JOIN exact e USING (id) LEFT JOIN fts f USING (id) LEFT JOIN vec v USING (id)
  WHERE e.id IS NOT NULL OR f.id IS NOT NULL OR v.id IS NOT NULL
  ORDER BY o LIMIT :lim) d;
