#!/usr/bin/env python3
"""Independent SQLite search oracle: FTS5 BM25 + Python cosine + reciprocal rank fusion."""
import json
import math
import re
import sqlite3
import struct
import sys
path,query,raw_vector,exact,kinds=sys.argv[1:]
vec=json.loads(raw_vector)
allowed=set(kinds.strip('{}').split(','))
with sqlite3.connect(path) as db:
    db.row_factory=sqlite3.Row
    docs={r['id']:r for r in db.execute('SELECT * FROM search_docs') if r['kind'] in allowed}
    exact_ids=sorted(i for i,r in docs.items() if exact and exact.lower() in (r['ident'] or '').lower())[:10]
    terms=' OR '.join('"'+w+'"' for w in re.findall(r'\w+',query)[:64]) or '""'
    text=[r[0] for r in db.execute('SELECT rowid,bm25(search_docs_fts,5.0,3.0,1.0) AS score FROM search_docs_fts WHERE search_docs_fts MATCH ? ORDER BY score,rowid',(terms,)) if r[0] in docs][:50]
    def distance(row):
        v=struct.unpack('<1536f',row['embedding'])
        return 1-sum(a*b for a,b in zip(v,vec))/math.sqrt(sum(a*a for a in v)*sum(b*b for b in vec))
    vectors=sorted((i for i,r in docs.items() if r['embedding'] is not None and r['embed_model']=='openai/text-embedding-3-small'),key=lambda i:(distance(docs[i]),i))[:50]
    e,f,v=({value:n+1 for n,value in enumerate(xs)} for xs in (exact_ids,text,vectors))
    ranked=sorted(e.keys()|f.keys()|v.keys(),key=lambda i:(i not in e,e.get(i,math.inf),-(1/(60+f[i]) if i in f else 0)-(1/(60+v[i]) if i in v else 0),i))[:8]
    print(','.join(docs[i]['ref'] for i in ranked))
