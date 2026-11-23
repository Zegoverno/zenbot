# Plugin host: real stdio fixture, bubblewrap, owner approval and UI audit.
plugins_host() {
  local ws; ws=$(new_workspace plugins)
  local files="$TMP/files" fixture="$REPO/tests/fixtures/plugins/hello"
  mkdir -p "$files"; echo vault-note > "$files/note"
  local market="$TMP/plugin-market.json"
  echo '{"plugins":[{"name":"hello","version":"0.1.0"}]}' > "$market"
  start_kernel "$ws" "" "ZEN_PLUGIN_FILES=$files" "ZEN_ASSIST_MODEL=faux/smoke" "ZEN_MARKETPLACE=$market"
  plugin_api() { curl -sS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
  plugin_call() { plugin_api -d "$(jq -nc --arg tool "$1" --argjson args "$2" '{tool:$tool,arguments:$args}')" "$URL/api/plugins/hello/call"; }
  local body r
  body=$(jq -nc --arg path "$fixture" '{path:$path}')
  r=$(plugin_api -d "$body" "$URL/api/plugins/install")
  check "permissions require approval" eq "$(jq -r .needs_approval <<<"$r")" true
  check "approval installs nothing" eq "$(q 'SELECT count(*) FROM plugins')" 0
  body=$(jq '. + {approve:true,permissions:["ai","files:read"]}' <<<"$body")
  r=$(plugin_api -d "$body" "$URL/api/plugins/install")
  check "fixture installed" eq "$(jq -r .name <<<"$r")" hello
  check "install assigns declared default" eq "$(plugin_api "$URL/api/plugins" | jq -r '[.plugins[0].contributes.games[0].id, .defaults["*.hello:read"]]|join("|")')" "guess|hello/open"
  r=$(plugin_call hello_greet '{"name":"World"}')
  check "real sandboxed stdio tool" eq "$(jq -r '.content[0].text' <<<"$r")" 'Hello, World!'
  check "UI call audited" eq "$(q "SELECT count(*) FROM plugin_calls WHERE caller='ui' AND tool='hello_greet' AND is_error=0")" 1
  r=$(plugin_api --get --data-urlencode uri=ui://hello/app "$URL/api/plugins/hello/resource")
  check "UI resource" eq "$(jq -r '.mimeType' <<<"$r")" 'text/html;profile=mcp-app'
  check "model-only tool refused" eq "$(plugin_api -o /dev/null -w '%{http_code}' -d '{"tool":"hello_hidden"}' "$URL/api/plugins/hello/call")" 403
  check "foreign tool refused" eq "$(plugin_api -o /dev/null -w '%{http_code}' -d '{"tool":"other_tool"}' "$URL/api/plugins/hello/call")" 404
  r=$(plugin_call hello_peek "$(jq -nc --arg path "$files/note" '{path:$path}')")
  check "granted vault read" eq "$(jq -r '.content[0].text' <<<"$r")" vault-note
  r=$(plugin_call hello_peek "$(jq -nc --arg path "$files/note" '{path:$path,text:"overwrite"}')")
  check "read-only vault" eq "$(jq -r .isError <<<"$r")" true
  r=$(plugin_call hello_peek "$(jq -nc --arg path "$TMP/home/.zenbot/token" '{path:$path}')")
  check "owner home hidden" eq "$(jq -r .isError <<<"$r")" true
  r=$(plugin_call hello_dial "$(jq -nc --argjson port "$PORT" '{host:"127.0.0.1",port:$port}')")
  check "no host loopback" eq "$(jq -r '.content[0].text == "connected"' <<<"$r")" false
  r=$(plugin_call hello_ask '{"prompt":"FACT: sampling works","maxTokens":8}')
  check "sampling answered with ai" eq "$(jq -r '[.isError, (.content[0].text|contains("FACT: sampling works"))]|join("|")' <<<"$r")" 'false|true'
  check "sampling audited with its model" eq "$(q "SELECT count(*) FROM plugin_calls WHERE caller='sampling' AND is_error=0 AND output_bytes>0 AND model='faux/smoke'")" 1
  check "AI calls counted" eq "$(plugin_api "$URL/api/plugins" | jq -r '.plugins[0]|[.ai_enabled,.ai_calls_30d]|join("|")')" 'true|1'
  r=$(plugin_api -X PATCH -d '{"ai":false}' "$URL/api/plugins/hello")
  check "AI switched off" eq "$(jq -r .ai_enabled <<<"$r")" false
  r=$(plugin_call hello_ask '{"prompt":"FACT: switched off"}')
  check "sampling refused when AI is off" eq "$(jq -r '[.isError, (.content[0].text|contains("AI is turned off for this plugin"))]|join("|")' <<<"$r")" 'true|true'
  check "refusal audited without generation" eq "$(q "SELECT count(*) FROM plugin_calls WHERE caller='sampling' AND is_error=1 AND model IS NULL")" 1
  plugin_api -X PATCH -d '{"ai":true}' "$URL/api/plugins/hello" >/dev/null
  plugin_api -X PUT -d '{"key":"*.hello:read","value":"hello/open"}' "$URL/api/plugins/defaults" >/dev/null
  plugin_api -X PATCH -d '{"enabled":false}' "$URL/api/plugins/hello" >/dev/null
  check "disabled server refused" eq "$(plugin_api -o /dev/null -w '%{http_code}' -d '{"tool":"hello_greet"}' "$URL/api/plugins/hello/call")" 409
  plugin_api -X PATCH -d '{"enabled":true}' "$URL/api/plugins/hello" >/dev/null
  plugin_api -X PUT -d '{"key":"*.hello:read","value":"builtin"}' "$URL/api/plugins/defaults" >/dev/null
  local dev="$TMP/plugin"
  cp -R "$fixture" "$dev"
  jq '.version="0.2.0" | .kernel=["files:write:Reading"]' "$dev/plugin.json" > "$dev/next.json"
  mv "$dev/next.json" "$dev/plugin.json"
  body=$(jq -nc --arg path "$dev" '{path:$path}')
  r=$(plugin_api -d "$body" "$URL/api/plugins/install")
  check "upgrade escalation needs new approval" eq "$(jq -r .needs_approval <<<"$r")" true
  check "unapproved upgrade leaves old version" eq "$(q "SELECT version FROM plugins WHERE name='hello'")" 0.1.0
  body=$(jq '. + {approve:true,permissions:["files:write:Reading"]}' <<<"$body")
  r=$(plugin_api -d "$body" "$URL/api/plugins/install")
  check "upgrade preserves chosen builtin" eq "$(plugin_api "$URL/api/plugins" | jq -r '.defaults["*.hello:read"]')" builtin
  plugin_api -X PUT -d '{"key":"*.hello:read","value":"hello/open"}' "$URL/api/plugins/defaults" >/dev/null
  check "approved upgrade" eq "$(jq -r .version <<<"$r")" 0.2.0
  check "stale marketplace entry is not offered" jq -e '.plugins[0] | .version == "0.1.0" and .installed == "0.2.0" and .update_available == false' <<<"$(plugin_api "$URL/api/plugins/marketplace")"
  r=$(plugin_api -w '\n%{http_code}' -d '{"name":"hello","approve":true,"permissions":["files:write:Reading"]}' "$URL/api/plugins/install")
  check "direct marketplace downgrade is refused before archive fetch" eq "$(tail -1 <<<"$r")" 422
  check "refusal explains version mismatch" jq -e '.error | contains("not newer than installed 0.2.0")' <<<"$(head -1 <<<"$r")"
  check "refusal leaves the upgraded plugin installed" eq "$(q "SELECT version FROM plugins WHERE name='hello'")" 0.2.0
  r=$(plugin_call hello_peek "$(jq -nc --arg path "$files/Reading/new" '{path:$path,text:"saved"}')")
  check "scoped write allowed" eq "$(jq -r '.content[0].text' <<<"$r")" written
  r=$(plugin_call hello_peek "$(jq -nc --arg path "$files/note" '{path:$path,text:"overwrite"}')")
  check "scoped write cannot write siblings" eq "$(jq -r .isError <<<"$r")" true
  r=$(plugin_call hello_ask '{"prompt":"not allowed"}')
  check "sampling denied without ai after upgrade" eq "$(jq -r .isError <<<"$r")" true
  check "CLI exposes enabled command" eq "$(ZEN_URL=$URL ZEN_TOKEN=$TOKEN "$BIN/zen" --commands | jq -r '.commands[]|select(.plugin=="hello")|.tool')" hello_greet
  check "CLI executes plugin command" eq "$(ZEN_URL=$URL ZEN_TOKEN=$TOKEN "$BIN/zen" --json hello Terminal | jq -r '.content[0].text')" 'Hello, Terminal!'
  stop_kernel
  start_kernel "$ws" "" "ZEN_PLUGIN_FILES=$files" "ZEN_ASSIST_MODEL=faux/smoke"
  r=$(plugin_call hello_greet '{"name":"Restart"}')
  check "installed plugins survive restart" eq "$(jq -r '.content[0].text' <<<"$r")" 'Hello, Restart!'
  plugin_api -X DELETE "$URL/api/plugins/hello" >/dev/null
  check "removed registry and defaults" eq "$(plugin_api "$URL/api/plugins" | jq -r '[(.plugins|length),(.defaults|length)]|join("|")')" '0|0'
  check "plugin data retained" test -f "$TMP/home/.zenbot/plugin-data/hello/started.json"
  jq '.kernel=[]' "$dev/plugin.json" > "$dev/next.json"
  mv "$dev/next.json" "$dev/plugin.json"
  body=$(jq -nc --arg path "$dev" '{path:$path}')
  r=$(plugin_api -d "$body" "$URL/api/plugins/install")
  check "even zero-permission code needs install consent" eq "$(jq -r .needs_approval <<<"$r")" true
  check "zero-permission preview installs nothing" eq "$(q 'SELECT count(*) FROM plugins')" 0
  r=$(plugin_api -d "$(jq '. + {approve:true,permissions:[]}' <<<"$body")" "$URL/api/plugins/install")
  check "explicit empty grant installs" eq "$(jq -r .name <<<"$r")" hello
  check "foreign resource refused" eq "$(plugin_api -o /dev/null -w '%{http_code}' --get --data-urlencode uri=ui://other/app "$URL/api/plugins/hello/resource")" 403
  check "missing owner token refused" eq "$(curl -sS -o /dev/null -w '%{http_code}' "$URL/api/plugins")" 401
}
run plugins plugins_host

# Explicit opt-in: public release downloads must never make ordinary CI network-dependent.
plugins_release_smoke() {
  local ws; ws=$(new_workspace plugin-releases)
  local files="$TMP/vault" market_port=$((PORT + 1)) market_pid
  mkdir -p "$files"
  printf '# Release smoke\nA public plugin reads this note.\n' > "$files/smoke.md"
  # Serve the current catalog unchanged: the kernel fetches the public archives and verifies
  # their pinned SHA256 itself (no local archive substitution or npm install).
  python3 -m http.server "$market_port" --bind 127.0.0.1 --directory "$REPO/marketplace" >"$TMP/market.log" 2>&1 &
  market_pid=$!
  trap 'kill "$market_pid" 2>/dev/null || true' RETURN
  until_ok curl -fsS -o /dev/null "http://127.0.0.1:$market_port/plugins.json" || return 1
  start_kernel "$ws" "" "ZEN_PLUGIN_FILES=$files" "ZEN_MARKETPLACE=http://127.0.0.1:$market_port/plugins.json"
  release_api() { curl -fsS --max-time 120 -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' "$@"; }
  release_call() { release_api -d "$(jq -nc --arg tool "$2" --argjson args "$3" '{tool:$tool,arguments:$args}')" "$URL/api/plugins/$1/call"; }
  local name body r permissions version resource before
  for name in zenread zensudoku zenwrite; do
    body=$(jq -nc --arg name "$name" '{name:$name}')
    before=$(q 'SELECT count(*) FROM plugins')
    r=$(release_api -d "$body" "$URL/api/plugins/install") || return 1
    permissions=$(jq -c --arg name "$name" '.plugins[]|select(.name==$name)|.kernel' "$REPO/marketplace/plugins.json")
    version=$(jq -r --arg name "$name" '.plugins[]|select(.name==$name)|.version' "$REPO/marketplace/plugins.json")
    check "$name release permissions preview" eq "$(jq -r .needs_approval <<<"$r")" true
    check "$name preview exact permissions" eq "$(jq -c '.permissions|sort' <<<"$r")" "$(jq -c sort <<<"$permissions")"
    check "$name preview installs nothing" eq "$(q 'SELECT count(*) FROM plugins')" "$before"
    body=$(jq --argjson permissions "$permissions" '. + {approve:true,permissions:$permissions}' <<<"$body")
    r=$(release_api -d "$body" "$URL/api/plugins/install") || return 1
    check "$name pinned release installed" eq "$(jq -r '[.name,.version]|join("|")' <<<"$r")" "$name|$version"
    check "$name bundled server exists" test -s "$TMP/home/.zenbot/plugins/$name/mcp/dist/server.js"
    check "$name has no node_modules" eq "$(find "$TMP/home/.zenbot/plugins/$name" -name node_modules -print -quit)" ""
    resource=$(release_api --get --data-urlencode "uri=ui://$name/app" "$URL/api/plugins/$name/resource") || return 1
    check "$name released MCP App resource readable" jq -e '.mimeType == "text/html;profile=mcp-app" and (.text|length > 100)' <<<"$resource"
  done
  r=$(release_call zenread zenread_list '{}') || return 1
  check "released reader lists vault note" jq -e '(.isError != true) and any(.structuredContent.files[]; .path == "smoke.md")' <<<"$r"
  r=$(release_call zenread zenread_open '{"path":"smoke.md"}') || return 1
  check "released reader opens vault note" eq "$(jq -r .structuredContent.text <<<"$r")" "$(cat "$files/smoke.md")"
  r=$(release_call zenwrite zenwrite_open '{"path":"smoke.md"}') || return 1
  check "released writer opens vault note" eq "$(jq -r .structuredContent.text <<<"$r")" "$(cat "$files/smoke.md")"
  body=$(jq -nc --arg hash "$(jq -r .structuredContent.hash <<<"$r")" '{path:"smoke.md",text:"# Saved by released zenwrite\n",base_hash:$hash}')
  r=$(release_call zenwrite zenwrite_save "$body") || return 1
  check "released writer saves without tool error" jq -e '.isError != true and .structuredContent.path == "smoke.md"' <<<"$r"
  check "released writer actually changed vault" eq "$(cat "$files/smoke.md")" '# Saved by released zenwrite'
  r=$(release_call zensudoku zensudoku_new '{}') || return 1
  check "released Sudoku opens example" jq -e '.isError != true and (.structuredContent.cells|length == 81) and .structuredContent.empty > 0' <<<"$r"
  body=$(jq -c .structuredContent <<<"$r")
  r=$(release_call zensudoku zensudoku_state '{}') || return 1
  check "released Sudoku retains board between real MCP calls" eq "$(jq -c .structuredContent <<<"$r")" "$body"
  check "release tools audited by kernel" eq "$(q "SELECT count(*) FROM plugin_calls WHERE caller='ui' AND is_error=0")" 6
  stop_kernel
  kill "$market_pid"; wait "$market_pid" 2>/dev/null || true
  trap - RETURN
}
if [ "${ZEN_PLUGIN_RELEASE_SMOKE:-}" = 1 ]; then
  run plugins_release plugins_release_smoke
fi
