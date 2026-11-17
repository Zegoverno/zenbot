# Learning loop: real archive API, faux review, structured submission and jobs/Matrix feed.
learning_loop() {
  local ws; ws=$(new_workspace learning)
  start_kernel "$ws" "$(script learning.json)" ZEN_REVIEW=1 ZEN_REVIEW_MODEL=faux/smoke
  mkdir -p "$TMP/home/.zenbot/global/skills"
  ln -s "$ws" "$TMP/home/.zenbot/global/skills/escape"
  q "INSERT INTO memories(id,text,source,agent) VALUES(90001,'Other agent memory','owner','garry')" >/dev/null
  local sid; sid=$(zen ask --json -m faux/smoke OWNER-LEARNING | jq -r .session_id)
  curl -fsS -X PATCH -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json'     -d '{"archived":true}' "$URL/api/sessions/$sid" >/dev/null
  check "plain archive never queues a review" eq "$(q "SELECT count(*) FROM sessions WHERE kind='review'")" 0
  sid=$(zen ask --json -m faux/smoke OWNER-LEARNING | jq -r .session_id)
  curl -fsS -X PATCH -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json'     -d '{"archived":true,"outcome":"done"}' "$URL/api/sessions/$sid" >/dev/null
  check "archive done records a submitted review" poll "SELECT count(*) FROM session_reviews WHERE session_id=X'${sid//-/}' AND outcome='done' AND kind_of_work='test'" 1
  check "nonempty report reaches the jobs feed" poll "SELECT count(*) FROM job_runs r JOIN jobs j ON j.id=r.job_id WHERE j.action='session-review' AND r.status='ok' AND r.output LIKE '%Proposed note%'" 1
  check "the review session is archived" poll "SELECT count(*) FROM sessions WHERE kind='review' AND archived" 1
  check "role writes and escaping skill saves were refused" eq "$(q "SELECT count(*) FROM tool_calls t JOIN sessions s ON s.id=t.session_id WHERE s.kind='review' AND t.name IN ('write','save_skill') AND t.is_error")" 2
  check "remember cannot remove another agent's memory" eq "$(q "SELECT tier FROM memories WHERE id=90001")" short
  check "escaping skill save wrote no files" test ! -e "$ws/forbidden/SKILL.md"
  check "plain archive added no review" eq "$(q "SELECT count(*) FROM session_reviews")" 1
  check "the feed Matrix polls includes the report" eq "$(curl -fsS -H "Authorization: Bearer $TOKEN" "$URL/api/jobs/runs" | jq '[.[] | select(.output | strings | contains("Proposed note"))] | length')" 1
}
