#!/usr/bin/env python3
"""Show how Apple's front end lowers MSL builtins: compiles a snippet with raw IRGen
(`metal -S -emit-llvm -Xclang -disable-llvm-passes`) and prints the bodies of the metal::
wrapper functions it calls, plus the air.* declarations. Used to write the builtin table of
air-emit by hand. Usage: probe.py 'decls' 'stmt; stmt;' [--full]"""
import os, re, subprocess, sys, tempfile

def tool(t):
    return subprocess.run(['xcrun', '-sdk', 'macosx', '-find', t], capture_output=True, text=True).stdout.strip()

SDK = subprocess.run(['xcrun', '-sdk', 'macosx', '--show-sdk-path'], capture_output=True, text=True).stdout.strip()
PRE = '#include <metal_stdlib>\nusing namespace metal;\n'

def compile_ll(src):
    d = tempfile.mkdtemp(prefix='probe')
    p = os.path.join(d, 'p.metal')
    open(p, 'w').write(src)
    r = subprocess.run([tool('metal'), '-c', '-O1', '-fmetal-math-mode=relaxed', '-fpreserve-invariance', '-fwrapv', '-Xclang', '-disable-llvm-passes',
                        '-S', '-emit-llvm', p, '-o', os.path.join(d, 'p.ll')], env={'SDKROOT': SDK, 'PATH': '/usr/bin:/bin'},
                       capture_output=True, text=True)
    if r.returncode:
        sys.exit(r.stderr)
    return open(os.path.join(d, 'p.ll')).read()

def main():
    full = '--full' in sys.argv
    args = [a for a in sys.argv[1:] if a != '--full']
    if args[0] == '-':
        src = PRE + sys.stdin.read()
        ll = compile_ll(src)
        return show(ll, full)
    decls, body = args[0], args[1]
    src = PRE + decls + '\nkernel void k(device float4* o [[buffer(0)]], device int4* oi [[buffer(1)]], uint gid [[thread_position_in_grid]]) {\n' + body + '\n}\n'
    ll = compile_ll(src)
    show(ll, full)

def show(ll, full):
    if full:
        print(ll); return
    funcs = re.findall(r'^define [^\n]*\n(?:.*\n)*?^}\n', ll, re.M)
    for f in funcs:
        name = re.search(r'@([\w.$]+)\(', f).group(1)
        print(f)
    for d in re.findall(r'^declare .*$', ll, re.M):
        if '@air.' in d or '@llvm.' in d:
            print(d)
    for a in re.findall(r'^attributes .*$', ll, re.M):
        print(a)

main()
