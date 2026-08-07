#!/usr/bin/env python3
"""Lightweight Den MCP wrapper for Muse when native mcp tools are not auto-populated.
Uses the same endpoint Muse is configured for: http://192.168.1.10:5199/mcp?tool_profile=planner
Allows: python3 /tmp/den_wrapper.py get_task --task_id 6629
"""
import json, sys, urllib.request, urllib.error, argparse
URL="http://192.168.1.10:5199/mcp?tool_profile=planner"
def call(tool, args):
    payload={"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}
    data=json.dumps(payload).encode()
    req=urllib.request.Request(URL, data=data, headers={"Content-Type":"application/json","Accept":"application/json, text/event-stream"}, method="POST")
    with urllib.request.urlopen(req, timeout=20) as r:
        body=json.loads(r.read().decode())
        if "error" in body:
            print(json.dumps(body, indent=2)); sys.exit(1)
        # pretty print structuredContent if present
        res=body["result"]
        sc=res.get("structuredContent")
        if sc:
            print(json.dumps(sc, indent=2))
        else:
            # fallback to content text
            for c in res.get("content",[]):
                print(c.get("text","")[:8000])
        # also dump raw for debugging if --raw
if __name__=="__main__":
    import argparse, json as j
    p=argparse.ArgumentParser()
    p.add_argument("tool")
    p.add_argument("--args", default="{}", help="JSON dict of arguments")
    p.add_argument("--arg", action="append", default=[], help="key=value (repeatable, alternative to --args)")
    args=p.parse_args()
    try:
        a=j.loads(args.args) if args.args else {}
    except: a={}
    for kv in args.arg:
        if "=" in kv:
            k,v=kv.split("=",1)
            # try json parse value
            try: v=j.loads(v)
            except: pass
            # try int
            if isinstance(v,str) and v.isdigit(): v=int(v)
            a[k]=v
    call(args.tool, a)
