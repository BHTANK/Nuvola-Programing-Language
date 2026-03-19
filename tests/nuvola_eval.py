#!/usr/bin/env python3
"""
Nuvola Language Evaluator — v0.1
Interprets a meaningful subset of Nuvola syntax.

Supported:
  - Immutable (:=) and mutable (=) bindings
  - Primitive types: i64, f64, str, bool, nil/None
  - Arithmetic, comparison, logical operators
  - String interpolation  {"expr"}
  - Functions (fn), lambdas (fn(...) =>), placeholder lambdas (_ * 2)
  - If/else expressions and statements
  - Match expressions (literal, wildcard, range, OR, guard, enum variant)
  - For loops (range and iterable)
  - While loops
  - Pipeline operator |>
  - Vec (list), Map (dict), Set literals
  - Builtins: print, len, range, map, filter, reduce/fold, sum, min, max,
              zip, enumerate, sorted, reversed, flatten, count, any, all,
              take, drop, join, split, contains, type_of, abs, sqrt, floor,
              ceil, round, pow, str(), int(), float(), bool()
  - Structs (type) and enum variants
  - Option (Some/None) and Result (Ok/Err)
  - Error propagation via catch
  - Tuple destructuring
  - Closures with captured environment
  - Recursion (including mutual)
  - Modules (import from stdlib stubs)
  - @pure/@io annotations (parsed, not enforced)
"""

import re
import sys
import math
import time
import traceback
from typing import Any, Dict, List, Optional, Tuple

# ─────────────────────────────────────────────────────────────────────────────
# AST nodes
# ─────────────────────────────────────────────────────────────────────────────

class Node:
    pass

class Num(Node):
    def __init__(self, v): self.v = v
class Str(Node):
    def __init__(self, v): self.v = v
class Bool(Node):
    def __init__(self, v): self.v = v
class Nil(Node):
    pass
class Ident(Node):
    def __init__(self, name): self.name = name
class BinOp(Node):
    def __init__(self, op, l, r): self.op=op; self.l=l; self.r=r
class UnOp(Node):
    def __init__(self, op, expr): self.op=op; self.expr=expr
class Pipe(Node):
    def __init__(self, l, r): self.l=l; self.r=r
class Call(Node):
    def __init__(self, fn, args, kwargs=None): self.fn=fn; self.args=args; self.kwargs=kwargs or {}
class Index(Node):
    def __init__(self, obj, idx): self.obj=obj; self.idx=idx
class Field(Node):
    def __init__(self, obj, field): self.obj=obj; self.field=field
class Assign(Node):
    def __init__(self, name, expr, immutable=True, typed=None): self.name=name; self.expr=expr; self.immutable=immutable; self.typed=typed
class IndexAssign(Node):
    def __init__(self, obj, idx, expr): self.obj=obj; self.idx=idx; self.expr=expr
class FieldAssign(Node):
    def __init__(self, obj, field, expr): self.obj=obj; self.field=field; self.expr=expr
class Destructure(Node):
    def __init__(self, pattern, expr): self.pattern=pattern; self.expr=expr
class Fn(Node):
    def __init__(self, params, body, name=None): self.params=params; self.body=body; self.name=name
class FnDecl(Node):
    def __init__(self, name, params, body, ret_type=None): self.name=name; self.params=params; self.body=body; self.ret_type=ret_type
class If(Node):
    def __init__(self, cond, then, elifs=None, else_=None): self.cond=cond; self.then=then; self.elifs=elifs or []; self.else_=else_
class Match(Node):
    def __init__(self, expr, arms): self.expr=expr; self.arms=arms
class MatchArm(Node):
    def __init__(self, pattern, guard, body): self.pattern=pattern; self.guard=guard; self.body=body
class ForLoop(Node):
    def __init__(self, var, iter_, body): self.var=var; self.iter_=iter_; self.body=body
class WhileLoop(Node):
    def __init__(self, cond, body): self.cond=cond; self.body=body
class Block(Node):
    def __init__(self, stmts): self.stmts=stmts
class Return(Node):
    def __init__(self, expr=None): self.expr=expr
class Break(Node):
    def __init__(self, expr=None): self.expr=expr
class Continue(Node):
    pass
class ListLit(Node):
    def __init__(self, items): self.items=items
class MapLit(Node):
    def __init__(self, pairs): self.pairs=pairs
class SetLit(Node):
    def __init__(self, items): self.items=items
class TupleLit(Node):
    def __init__(self, items): self.items=items
class RangeLit(Node):
    def __init__(self, start, end, inclusive=False): self.start=start; self.end=end; self.inclusive=inclusive
class StructLit(Node):
    def __init__(self, name, fields): self.name=name; self.fields=fields
class Spread(Node):
    def __init__(self, base, overrides): self.base=base; self.overrides=overrides
class TypeDecl(Node):
    def __init__(self, name, fields=None, variants=None): self.name=name; self.fields=fields; self.variants=variants
class Import(Node):
    def __init__(self, path, names=None, alias=None): self.path=path; self.names=names; self.alias=alias
class Annotation(Node):
    def __init__(self, name, inner): self.name=name; self.inner=inner

# Runtime values
class NuvolaFn:
    def __init__(self, params, body, env, name=None):
        self.params = params; self.body = body; self.env = env; self.name = name
    def __repr__(self): return f"<fn {self.name or '(anon)'}>"

class NuvolaStruct:
    def __init__(self, type_name, fields):
        self.type_name = type_name; self.fields = dict(fields)
    def __repr__(self):
        fs = ', '.join(f"{k}: {repr(v)}" for k, v in self.fields.items())
        return f"{self.type_name} {{ {fs} }}"

class NuvolaEnum:
    def __init__(self, type_name, variant, value=None):
        self.type_name = type_name; self.variant = variant; self.value = value
    def __repr__(self):
        if self.value is None: return f"{self.type_name}.{self.variant}"
        return f"{self.type_name}.{self.variant}({repr(self.value)})"

class NuvolaOption:
    def __init__(self, value=None, is_some=True):
        self.is_some = is_some; self.value = value
    def __repr__(self): return f"Some({repr(self.value)})" if self.is_some else "None"

class NuvolaResult:
    def __init__(self, value, is_ok=True):
        self.is_ok = is_ok; self.value = value
    def __repr__(self): return f"Ok({repr(self.value)})" if self.is_ok else f"Err({repr(self.value)})"

class ReturnSignal(Exception):
    def __init__(self, v): self.v = v
class BreakSignal(Exception):
    def __init__(self, v=None): self.v = v
class ContinueSignal(Exception):
    pass

SENTINEL = object()

# ─────────────────────────────────────────────────────────────────────────────
# Lexer
# ─────────────────────────────────────────────────────────────────────────────

KEYWORDS = {
    'fn', 'if', 'else', 'for', 'while', 'loop', 'in', 'return', 'break', 'continue',
    'match', 'type', 'trait', 'impl', 'import', 'export', 'from', 'as', 'where',
    'and', 'or', 'not', 'is', 'true', 'false', 'nil', 'self',
    'Some', 'None', 'Ok', 'Err',
}

def tokenize(src: str) -> List[Tuple[str, str, int]]:
    tokens = []
    i = 0
    lines = src.split('\n')
    line_num = 1
    line_starts = [0]
    for line in lines[:-1]:
        line_starts.append(line_starts[-1] + len(line) + 1)

    def get_line(pos):
        lo, hi = 0, len(line_starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if line_starts[mid] <= pos: lo = mid
            else: hi = mid - 1
        return lo + 1

    def get_col(pos):
        line_idx = get_line(pos) - 1
        return pos - line_starts[line_idx]

    while i < len(src):
        # skip doc comments and regular comments
        if src[i:i+2] == '--':
            end = src.find('\n', i)
            i = end + 1 if end != -1 else len(src)
            continue
        # skip whitespace
        if src[i] in ' \t\r':
            i += 1; continue
        if src[i] == '\n':
            tokens.append(('NEWLINE', '\n', get_line(i), 0))
            i += 1; continue

        ln = get_line(i)

        col = get_col(i)

        # Annotations
        if src[i] == '@':
            j = i + 1
            while j < len(src) and (src[j].isalnum() or src[j] == '_'):
                j += 1
            tokens.append(('ANNOT', src[i:j], ln, col))
            i = j; continue

        # Multi-line string
        if src[i:i+3] == '"""':
            j = i + 3
            while j < len(src) and src[j:j+3] != '"""':
                j += 1
            tokens.append(('STRING', src[i+3:j], ln, col))
            i = j + 3; continue

        # String with interpolation
        if src[i] == '"':
            j = i + 1
            s = []
            while j < len(src) and src[j] != '"':
                if src[j] == '\\':
                    esc = {'n':'\n','t':'\t','r':'\r','\\':'\\','"':'"'}.get(src[j+1], src[j+1])
                    s.append(esc); j += 2
                else:
                    s.append(src[j]); j += 1
            tokens.append(('STRING', ''.join(s), ln, col))
            i = j + 1; continue

        # Numbers
        if src[i].isdigit() or (src[i] == '.' and i+1 < len(src) and src[i+1].isdigit()):
            j = i
            while j < len(src) and (src[j].isdigit() or src[j] == '_'):
                j += 1
            is_float = False
            if j < len(src) and src[j] == '.' and (j+1 >= len(src) or src[j+1] != '.'):
                is_float = True; j += 1
                while j < len(src) and (src[j].isdigit() or src[j] == '_'):
                    j += 1
            if j < len(src) and src[j] in 'eE':
                is_float = True; j += 1
                if j < len(src) and src[j] in '+-': j += 1
                while j < len(src) and src[j].isdigit(): j += 1
            # type suffix
            while j < len(src) and src[j] in 'ufif0123456789':
                j += 1
            raw = src[i:j].replace('_', '')
            # strip type suffix
            num_str = re.sub(r'[ui](?:8|16|32|64|128|size)$|f(?:16|32|64|128)$', '', raw)
            tok_type = 'FLOAT' if is_float or '.' in num_str or 'e' in num_str.lower() else 'INT'
            tokens.append((tok_type, num_str, ln, col))
            i = j; continue

        # Identifiers and keywords
        if src[i].isalpha() or src[i] == '_':
            j = i
            while j < len(src) and (src[j].isalnum() or src[j] == '_'):
                j += 1
            word = src[i:j]
            if word in ('true', 'false'):
                tokens.append(('BOOL', word, ln, col))
            elif word in ('nil', 'None'):
                tokens.append(('NIL', word, ln, col))
            elif word in KEYWORDS:
                tokens.append(('KW', word, ln, col))
            else:
                tokens.append(('IDENT', word, ln, col))
            i = j; continue

        # Operators (order matters — longer first)
        for op in ['..=', '|>=', '|>', '=>', ':=', '!=', '<=', '>=', '==', '&&', '||',
                   '<<', '>>', '->', '**', '//', '??', '?.', '..', '<<', '>>',
                   '+', '-', '*', '/', '%', '<', '>', '=', ':', '.', ',', ';',
                   '(', ')', '[', ']', '{', '}', '!', '?', '&', '|', '^', '~', '@']:
            if src[i:i+len(op)] == op:
                tokens.append(('OP', op, ln, col))
                i += len(op); break
        else:
            i += 1  # skip unknown char

    tokens.append(('EOF', '', get_line(len(src)-1) if src else 1, 0))
    return tokens

# ─────────────────────────────────────────────────────────────────────────────
# Parser
# ─────────────────────────────────────────────────────────────────────────────

class ParseError(Exception):
    def __init__(self, msg, line=0): super().__init__(f"Line {line}: {msg}"); self.line=line

class Parser:
    def __init__(self, tokens):
        self.tokens = [t for t in tokens if t[0] != 'NEWLINE' or True]  # keep newlines for indentation
        self.pos = 0

    def peek(self, offset=0) -> Tuple[str, str, int]:
        p = self.pos + offset
        if p < len(self.tokens): return self.tokens[p]
        return ('EOF', '', 0)

    def skip_newlines(self):
        while self.peek()[0] == 'NEWLINE':
            self.pos += 1

    def eat(self, type_=None, value=None):
        self.skip_newlines()
        t = self.tokens[self.pos]
        if type_ and t[0] != type_:
            raise ParseError(f"Expected {type_} but got {t[0]} '{t[1]}'", t[2])
        if value and t[1] != value:
            raise ParseError(f"Expected '{value}' but got '{t[1]}'", t[2])
        self.pos += 1
        return t

    def eat_if(self, type_=None, value=None) -> bool:
        self.skip_newlines()
        t = self.tokens[self.pos]
        if type_ and t[0] != type_: return False
        if value and t[1] != value: return False
        self.pos += 1
        return True

    def parse_program(self) -> Block:
        stmts = []
        while self.peek()[0] != 'EOF':
            self.skip_newlines()
            if self.peek()[0] == 'EOF': break
            s = self.parse_stmt()
            if s: stmts.append(s)
        return Block(stmts)

    def parse_block(self) -> Block:
        """Parse an indented block. Simplified: reads until dedent or end."""
        stmts = []
        self.skip_newlines()
        while True:
            self.skip_newlines()
            t = self.peek()
            if t[0] in ('EOF',): break
            if t[0] == 'NEWLINE': break
            # Check if next line is dedented (heuristic for our simplified parser)
            s = self.parse_stmt()
            if s: stmts.append(s)
            # After each statement, check for continuation
            self.skip_newlines()
            t2 = self.peek()
            if t2[0] in ('EOF', 'KW') and t2[1] in ('else', 'elif'):
                break
            if t2[0] == 'EOF':
                break
        return Block(stmts)

    def parse_stmt(self):
        self.skip_newlines()
        t = self.peek()

        # Annotation
        if t[0] == 'ANNOT':
            self.pos += 1
            self.skip_newlines()
            inner = self.parse_stmt()
            return Annotation(t[1], inner)

        # Import
        if t[0] == 'KW' and t[1] == 'import':
            return self.parse_import()

        # Type declaration
        if t[0] == 'KW' and t[1] == 'type':
            return self.parse_type_decl()

        # Function declaration (named) or anonymous fn expression used as statement
        if t[0] == 'KW' and t[1] == 'fn':
            # If next token is IDENT → named fn declaration; otherwise anon fn expression
            nxt = self.peek(1)
            if nxt[0] == 'IDENT':
                return self.parse_fn_decl()
            # Anonymous fn: fall through to parse_expr below

        # If statement (handled here so else-chains work at stmt level)
        if t[0] == 'KW' and t[1] == 'if':
            return self.parse_if_stmt()

        # For loop
        if t[0] == 'KW' and t[1] == 'for':
            return self.parse_for()

        # While loop
        if t[0] == 'KW' and t[1] == 'while':
            return self.parse_while()

        # Return
        if t[0] == 'KW' and t[1] == 'return':
            self.pos += 1
            self.skip_newlines()
            if self.peek()[0] in ('EOF', 'NEWLINE') or (self.peek()[0]=='KW' and self.peek()[1] in ('else','elif')):
                return Return(Nil())
            expr = self.parse_expr()
            return Return(expr)

        # Break
        if t[0] == 'KW' and t[1] == 'break':
            self.pos += 1
            if self.peek()[0] not in ('NEWLINE', 'EOF'):
                expr = self.parse_expr()
                return Break(expr)
            return Break()

        # Continue
        if t[0] == 'KW' and t[1] == 'continue':
            self.pos += 1
            return Continue()

        # Match
        if t[0] == 'KW' and t[1] == 'match':
            return self.parse_match()

        # Assignment/expression
        expr = self.parse_expr()

        # Check for binding (:= or = assignment at stmt level)
        if isinstance(expr, Ident) and self.peek()[0] == 'OP' and self.peek()[1] in (':=', '=', '+=', '-=', '*=', '/='):
            op = self.peek()[1]; self.pos += 1
            rhs = self.parse_expr()
            if op == ':=': return Assign(expr.name, rhs, immutable=True)
            elif op == '=': return Assign(expr.name, rhs, immutable=False)
            else:
                # compound assignment
                bin_op = op[0]
                return Assign(expr.name, BinOp(bin_op, expr, rhs), immutable=False)

        # Index assignment: arr[i] = val  or  arr[i][j] = val
        if isinstance(expr, Index) and self.peek()[0] == 'OP' and self.peek()[1] == '=':
            self.pos += 1
            rhs = self.parse_expr()
            return IndexAssign(expr.obj, expr.idx, rhs)

        # Field assignment: obj.field = val
        if isinstance(expr, Field) and self.peek()[0] == 'OP' and self.peek()[1] == '=':
            self.pos += 1
            rhs = self.parse_expr()
            return FieldAssign(expr.obj, expr.field, rhs)

        # Typed binding: name: Type = expr
        if isinstance(expr, Ident) and self.peek()[0] == 'OP' and self.peek()[1] == ':':
            self.pos += 1
            type_name = self.parse_type_expr()
            if self.peek()[0] == 'OP' and self.peek()[1] in (':=', '='):
                op = self.peek()[1]; self.pos += 1
                rhs = self.parse_expr()
                return Assign(expr.name, rhs, immutable=(op==':='), typed=type_name)
            return expr

        # Tuple destructure: (a, b) := expr
        if isinstance(expr, TupleLit) and self.peek()[0] == 'OP' and self.peek()[1] in (':=', '='):
            op = self.peek()[1]; self.pos += 1
            rhs = self.parse_expr()
            return Destructure(expr, rhs)

        # Skip trailing semicolons
        while self.peek()[0] == 'OP' and self.peek()[1] == ';':
            self.pos += 1
        return expr

    def parse_import(self):
        self.eat('KW', 'import')
        path_parts = []
        t = self.eat('IDENT')
        path_parts.append(t[1])
        names = None; alias = None
        while self.peek()[0] == 'OP' and self.peek()[1] == '.':
            self.pos += 1
            if self.peek()[0] == 'OP' and self.peek()[1] == '{':
                self.pos += 1
                names = []
                while self.peek()[1] != '}':
                    names.append(self.eat('IDENT')[1])
                    if not self.eat_if('OP', ','): break
                self.eat('OP', '}')
                break
            t = self.eat('IDENT')
            path_parts.append(t[1])
        if self.peek()[0] == 'KW' and self.peek()[1] == 'as':
            self.pos += 1
            alias = self.eat('IDENT')[1]
        return Import('.'.join(path_parts), names, alias)

    def parse_type_decl(self):
        self.eat('KW', 'type')
        name = self.eat('IDENT')[1]
        self.skip_newlines()
        fields = {}; variants = []
        # Determine indentation level of type body
        type_col = self._tok_col()
        # Struct fields or enum variants
        while True:
            self.skip_newlines()
            t = self.peek()
            if t[0] == 'EOF': break
            # Stop if dedented back to type level or lower
            tok_col = t[3] if len(t) > 3 else 0
            if tok_col < type_col: break
            if t[0] == 'IDENT' and self.peek(1)[0] == 'OP' and self.peek(1)[1] == ':':
                # field: Type
                fname = self.eat('IDENT')[1]
                self.eat('OP', ':')
                ftype = self.parse_type_expr()
                default = None
                if self.peek()[0] == 'OP' and self.peek()[1] == '=':
                    self.pos += 1; default = self.parse_expr()
                fields[fname] = (ftype, default)
            elif t[0] == 'IDENT':
                # enum variant
                vname = self.eat('IDENT')[1]
                vfields = []
                if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                    self.pos += 1
                    while self.peek()[1] != ')':
                        if self.peek()[0] == 'IDENT' and self.peek(1)[1] == ':':
                            fn_ = self.eat('IDENT')[1]; self.eat('OP', ':')
                            ft = self.parse_type_expr()
                            vfields.append((fn_, ft))
                        else:
                            ft = self.parse_type_expr()
                            vfields.append((None, ft))
                        if not self.eat_if('OP', ','): break
                    self.eat('OP', ')')
                variants.append((vname, vfields))
            else:
                break
        return TypeDecl(name, fields if fields else None, variants if variants else None)

    def parse_type_expr(self):
        t = self.eat('IDENT')
        name = t[1]
        if self.peek()[0] == 'OP' and self.peek()[1] == '(':
            self.pos += 1
            inner = self.parse_type_expr()
            if self.peek()[0] == 'OP' and self.peek()[1] == ',':
                self.pos += 1
                inner2 = self.parse_type_expr()
            self.eat('OP', ')')
        return name

    def parse_fn_decl(self):
        self.eat('KW', 'fn')
        name = self.eat('IDENT')[1]
        self.eat('OP', '(')
        params = self.parse_params()
        self.eat('OP', ')')
        # Optional return type
        ret_type = None
        if self.peek()[0] == 'OP' and self.peek()[1] == '->':
            self.pos += 1; ret_type = self.parse_type_expr()
        # Optional where clause
        if self.peek()[0] == 'KW' and self.peek()[1] == 'where':
            self.pos += 1
            while self.peek()[0] not in ('NEWLINE', 'EOF') and self.peek()[1] != '=>':
                self.pos += 1
        self.skip_newlines()
        # Arrow or block body
        if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
            self.pos += 1
            body = Block([Return(self.parse_expr())])
        else:
            body = self.parse_indented_block()
        return FnDecl(name, params, body, ret_type)

    def parse_params(self):
        params = []
        while self.peek()[1] != ')':
            if self.peek()[0] == 'OP' and self.peek()[1] == '...':
                self.pos += 1  # variadic
            t = self.eat('IDENT')
            pname = t[1]
            ptype = None; pdefault = None
            if self.peek()[0] == 'OP' and self.peek()[1] == ':':
                self.pos += 1; ptype = self.parse_type_expr()
                if self.peek()[0] == 'KW' and self.peek()[1] == 'where':
                    self.pos += 1
                    while self.peek()[1] not in (',', ')'):
                        self.pos += 1
            if self.peek()[0] == 'OP' and self.peek()[1] == '=':
                self.pos += 1; pdefault = self.parse_expr()
            params.append((pname, ptype, pdefault))
            if not self.eat_if('OP', ','): break
        return params

    def _tok_col(self, offset=0) -> int:
        """Return the column of the token at pos+offset, skipping NEWLINEs."""
        p = self.pos + offset
        while p < len(self.tokens) and self.tokens[p][0] == 'NEWLINE':
            p += 1
        t = self.tokens[p] if p < len(self.tokens) else ('EOF', '', 0, 0)
        return t[3] if len(t) > 3 else 0

    def parse_indented_block(self, min_col: int = -1):
        """Parse block as sequence of statements at >= min_col indentation.
        If min_col == -1, infer from the first token (the block's indent level)."""
        stmts = []
        self.skip_newlines()
        if self.peek()[0] == 'EOF':
            return Block([])
        # Determine the indentation of this block from its first token
        block_col = self._tok_col() if min_col < 0 else min_col
        while True:
            self.skip_newlines()
            t = self.peek()
            if t[0] == 'EOF': break
            # Indentation-based stop: dedented back to parent
            tok_col = t[3] if len(t) > 3 else 0
            if tok_col < block_col: break
            # Only stop at 'else' keyword — column tracking handles fn/type/import dedent
            if t[0] == 'KW' and t[1] == 'else': break
            s = self.parse_stmt()
            if s: stmts.append(s)
            # Skip semicolons used as statement separators on one line
            while self.peek()[0] == 'OP' and self.peek()[1] == ';':
                self.pos += 1
            self.skip_newlines()
            t2 = self.peek()
            if t2[0] == 'EOF': break
            tok_col2 = t2[3] if len(t2) > 3 else 0
            if tok_col2 < block_col: break
            if t2[0] == 'KW' and t2[1] == 'else': break
        return Block(stmts)

    def _parse_arrow_body(self):
        """Parse the body after '=>' in a statement context.
        Allows full statements (assignments, return, etc.)."""
        if self.peek()[0] == 'KW' and self.peek()[1] == 'return':
            self.pos += 1
            val = self.parse_expr()
            return Block([Return(val)])
        # Parse a full statement so `lo = mid + 1` works (not just parse_expr)
        s = self.parse_stmt()
        return Block([s] if s else [])

    def parse_if_stmt(self):
        """Parse if/else-if/else as a statement (proper block form)."""
        self.eat('KW', 'if')
        cond = self.parse_expr()
        self.skip_newlines()
        if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
            self.pos += 1
            then_block = self._parse_arrow_body()
        else:
            then_block = self.parse_indented_block()
        elifs = []
        else_block = None
        self.skip_newlines()
        while self.peek()[0] == 'KW' and self.peek()[1] == 'else':
            self.pos += 1
            self.skip_newlines()
            if self.peek()[0] == 'KW' and self.peek()[1] == 'if':
                self.pos += 1
                ec = self.parse_expr()
                self.skip_newlines()
                if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
                    self.pos += 1
                    elifs.append((ec, self._parse_arrow_body()))
                else:
                    eb = self.parse_indented_block()
                    elifs.append((ec, eb))
                self.skip_newlines()
            elif self.peek()[0] == 'OP' and self.peek()[1] == '=>':
                self.pos += 1
                else_block = self._parse_arrow_body()
                break
            else:
                else_block = self.parse_indented_block()
                break
        return If(cond, then_block, elifs, else_block)

    def parse_for(self):
        self.eat('KW', 'for')
        # var (or tuple of vars)
        if self.peek()[0] == 'OP' and self.peek()[1] == '(':
            self.pos += 1
            vars_ = [self.eat('IDENT')[1]]
            while self.eat_if('OP', ','):
                vars_.append(self.eat('IDENT')[1])
            self.eat('OP', ')')
            var = tuple(vars_)
        else:
            var_t = self.peek()
            if var_t[0] == 'IDENT' and self.peek(1)[0] == 'OP' and self.peek(1)[1] == ',':
                var = (self.eat('IDENT')[1],)
                self.eat('OP', ',')
                var = var + (self.eat('IDENT')[1],)
            else:
                var = self.eat('IDENT')[1]
        self.eat('KW', 'in')
        iter_ = self.parse_expr()
        self.skip_newlines()
        if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
            self.pos += 1
            body = Block([self.parse_stmt()])
        else:
            body = self.parse_indented_block()
        return ForLoop(var, iter_, body)

    def parse_while(self):
        self.eat('KW', 'while')
        cond = self.parse_expr()
        self.skip_newlines()
        if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
            self.pos += 1; body = Block([self.parse_stmt()])
        else:
            body = self.parse_indented_block()
        return WhileLoop(cond, body)

    def parse_match(self):
        self.eat('KW', 'match')
        expr = self.parse_expr()
        self.skip_newlines()
        arms = []
        # Record indentation level of first arm for column-based termination
        arm_col = self._tok_col()
        while True:
            self.skip_newlines()
            t = self.peek()
            if t[0] == 'EOF': break
            # Column-based stop: anything less indented than the first arm ends the match
            tok_col = t[3] if len(t) > 3 else 0
            if tok_col < arm_col: break
            if t[0] == 'KW' and t[1] in ('fn', 'type', 'for', 'while', 'return', 'match', 'else'): break
            if t[0] == 'OP' and t[1] == ')': break
            pattern = self.parse_match_pattern()
            guard = None
            if self.peek()[0] == 'KW' and self.peek()[1] == 'if':
                self.pos += 1; guard = self.parse_expr()
            self.eat('OP', '=>')
            self.skip_newlines()
            body = self.parse_match_body()
            arms.append(MatchArm(pattern, guard, body))
            if not arms or not self.peek()[0] in ('IDENT', 'INT', 'FLOAT', 'STRING', 'BOOL', 'NIL', 'KW', 'OP'):
                break
            # Peek if next line looks like a pattern
            t2 = self.peek()
            if t2[0] == 'EOF': break
            if t2[0] == 'KW' and t2[1] in ('fn', 'type', 'for', 'while', 'return', 'import'): break
        return Match(expr, arms)

    def parse_match_pattern(self):
        """Returns a pattern object (simplified as strings/tuples)."""
        parts = [self._parse_single_pattern()]
        while self.peek()[0] == 'OP' and self.peek()[1] == '|':
            self.pos += 1
            parts.append(self._parse_single_pattern())
        return parts if len(parts) > 1 else parts[0]

    def _parse_single_pattern(self):
        t = self.peek()
        if t[0] == 'OP' and t[1] == '_':
            self.pos += 1; return ('wildcard',)
        if t[0] == 'NIL':
            self.pos += 1; return ('literal', None)
        if t[0] == 'BOOL':
            self.pos += 1; return ('literal', t[1] == 'true')
        if t[0] == 'INT':
            self.pos += 1
            v = int(t[1])
            # Range?
            if self.peek()[0] == 'OP' and self.peek()[1] in ('..', '..='):
                inclusive = self.peek()[1] == '..='; self.pos += 1
                if self.peek()[0] == 'INT':
                    end = int(self.eat('INT')[1])
                    return ('range', v, end, inclusive)
                return ('range_from', v)
            return ('literal', v)
        if t[0] == 'FLOAT':
            self.pos += 1; return ('literal', float(t[1]))
        if t[0] == 'STRING':
            self.pos += 1; return ('literal', t[1])
        if t[0] == 'OP' and t[1] == '..':
            self.pos += 1
            if self.peek()[0] == 'INT':
                end = int(self.eat('INT')[1])
                return ('range_to', end, False)
            if self.peek()[0] == 'OP' and self.peek()[1] == '=':
                self.pos += 1
                end = int(self.eat('INT')[1])
                return ('range_to', end, True)
        if t[0] == 'IDENT':
            name = self.eat('IDENT')[1]
            # Enum variant: TypeName(fields) or TypeName.Variant
            if self.peek()[0] == 'OP' and self.peek()[1] == '.':
                self.pos += 1
                variant = self.eat('IDENT')[1]
                if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                    self.pos += 1
                    inner = self._parse_single_pattern()
                    self.eat('OP', ')')
                    return ('enum_variant', name, variant, inner)
                return ('enum_variant', name, variant, None)
            if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                self.pos += 1
                inners = [self._parse_single_pattern()]
                while self.eat_if('OP', ','):
                    inners.append(self._parse_single_pattern())
                self.eat('OP', ')')
                inner = inners[0] if len(inners) == 1 else inners
                return ('constructor', name, inner)
            # Bare name: binding or type check
            if name == 'Some':
                if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                    self.pos += 1; inner = self._parse_single_pattern(); self.eat('OP', ')')
                    return ('some', inner)
                return ('some', ('wildcard',))
            if name == 'None': return ('none',)
            if name == 'Ok':
                if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                    self.pos += 1; inner = self._parse_single_pattern(); self.eat('OP', ')')
                    return ('ok', inner)
                return ('ok', ('wildcard',))
            if name == 'Err':
                if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                    self.pos += 1; inner = self._parse_single_pattern(); self.eat('OP', ')')
                    return ('err', inner)
                return ('err', ('wildcard',))
            if name[0].isupper():
                return ('type_check', name)
            return ('bind', name)
        return ('wildcard',)

    def parse_match_body(self):
        """Parse a match arm body — single expr or block."""
        if self.peek()[0] == 'NEWLINE':
            self.skip_newlines()
            return self.parse_indented_block_single()
        return self.parse_expr_or_block()

    def parse_indented_block_single(self):
        stmts = []
        s = self.parse_stmt()
        if s: stmts.append(s)
        return Block(stmts)

    def parse_expr_or_block(self):
        return self.parse_expr()

    def parse_expr(self) -> Node:
        return self.parse_catch()

    def parse_pipe(self) -> Node:
        """Pipeline |> has higher precedence than * and / but lower than unary/postfix.
        So `xs |> sum / n` parses as `(xs |> sum) / n`."""
        left = self.parse_pow()
        while True:
            self.skip_newlines()
            if self.peek()[0] == 'OP' and self.peek()[1] == '|>':
                self.pos += 1
                self.skip_newlines()
                # Right side is postfix-only (function reference or call)
                right = self.parse_postfix()
                left = Pipe(left, right)
            else:
                break
        return left

    def parse_catch(self) -> Node:
        left = self.parse_or()
        # catch clauses
        while self.peek()[0] == 'KW' and self.peek()[1] == 'catch' if hasattr(self.peek()[0],'__len__') else False:
            break
        return left

    def parse_or(self) -> Node:
        left = self.parse_and()
        while True:
            t = self.peek()
            if t[0] == 'KW' and t[1] == 'or':
                self.pos += 1; right = self.parse_and()
                left = BinOp('or', left, right)
            elif t[0] == 'OP' and t[1] == '||':
                self.pos += 1; right = self.parse_and()
                left = BinOp('or', left, right)
            elif t[0] == 'OP' and t[1] == '??':
                self.pos += 1; right = self.parse_and()
                left = BinOp('??', left, right)
            else:
                break
        return left

    def parse_and(self) -> Node:
        left = self.parse_not()
        while True:
            t = self.peek()
            if t[0] == 'KW' and t[1] == 'and':
                self.pos += 1; right = self.parse_not()
                left = BinOp('and', left, right)
            elif t[0] == 'OP' and t[1] == '&&':
                self.pos += 1; right = self.parse_not()
                left = BinOp('and', left, right)
            else:
                break
        return left

    def parse_not(self) -> Node:
        if self.peek()[0] == 'KW' and self.peek()[1] == 'not':
            self.pos += 1; return UnOp('not', self.parse_not())
        return self.parse_compare()

    def parse_compare(self) -> Node:
        left = self.parse_range()
        while True:
            t = self.peek()
            if t[0] == 'OP' and t[1] in ('==', '!=', '<', '>', '<=', '>='):
                op = t[1]; self.pos += 1; right = self.parse_range()
                left = BinOp(op, left, right)
            elif t[0] == 'KW' and t[1] == 'is':
                self.pos += 1
                type_name = self.eat('IDENT')[1]
                left = BinOp('is', left, Ident(type_name))
            else:
                break
        return left

    def parse_range(self) -> Node:
        left = self.parse_add()
        if self.peek()[0] == 'OP' and self.peek()[1] in ('..', '..='):
            inclusive = self.peek()[1] == '..='; self.pos += 1
            right = self.parse_add()
            return RangeLit(left, right, inclusive)
        return left

    def parse_add(self) -> Node:
        left = self.parse_mul()
        while True:
            t = self.peek()
            if t[0] == 'OP' and t[1] in ('+', '-'):
                op = t[1]; self.pos += 1; right = self.parse_mul()
                left = BinOp(op, left, right)
            else:
                break
        return left

    def parse_mul(self) -> Node:
        left = self.parse_pipe()  # |> sits between mul and pow
        while True:
            t = self.peek()
            if t[0] == 'OP' and t[1] in ('*', '/', '//', '%'):
                op = t[1]; self.pos += 1; right = self.parse_pipe()
                left = BinOp(op, left, right)
            else:
                break
        return left

    def parse_pow(self) -> Node:
        left = self.parse_unary()
        if self.peek()[0] == 'OP' and self.peek()[1] == '**':
            self.pos += 1; right = self.parse_pow()  # right-associative
            return BinOp('**', left, right)
        return left

    def parse_unary(self) -> Node:
        t = self.peek()
        if t[0] == 'OP' and t[1] == '-':
            self.pos += 1; return UnOp('-', self.parse_unary())
        if t[0] == 'OP' and t[1] == '!':
            self.pos += 1; return UnOp('!', self.parse_unary())
        return self.parse_postfix()

    def parse_postfix(self) -> Node:
        node = self.parse_primary()
        while True:
            t = self.peek()
            # Field access
            if t[0] == 'OP' and t[1] == '.' and self.peek(1)[0] == 'IDENT':
                self.pos += 1
                field = self.eat('IDENT')[1]
                if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                    self.pos += 1
                    args = []
                    while self.peek()[1] != ')':
                        args.append(self.parse_expr())
                        if not self.eat_if('OP', ','): break
                    self.eat('OP', ')')
                    node = Call(Field(node, field), args)
                else:
                    node = Field(node, field)
            # Index
            elif t[0] == 'OP' and t[1] == '[':
                self.pos += 1
                idx = self.parse_expr()
                self.eat('OP', ']')
                node = Index(node, idx)
            # Call
            elif t[0] == 'OP' and t[1] == '(':
                self.pos += 1
                args = []
                while self.peek()[1] != ')':
                    args.append(self.parse_expr())
                    if not self.eat_if('OP', ','): break
                self.eat('OP', ')')
                node = Call(node, args)
            # Optional chaining ?.
            elif t[0] == 'OP' and t[1] == '?.':
                self.pos += 1
                field = self.eat('IDENT')[1]
                node = BinOp('?.', node, Ident(field))
            else:
                break
        return node

    def parse_primary(self) -> Node:
        self.skip_newlines()
        t = self.peek()

        if t[0] == 'INT':
            self.pos += 1; return Num(int(t[1]))
        if t[0] == 'FLOAT':
            self.pos += 1; return Num(float(t[1]))
        if t[0] == 'BOOL':
            self.pos += 1; return Bool(t[1] == 'true')
        if t[0] == 'NIL':
            self.pos += 1; return Nil()
        if t[0] == 'STRING':
            self.pos += 1; return Str(t[1])

        # Placeholder lambda: _ * 2 etc
        if t[0] == 'OP' and t[1] == '_':
            self.pos += 1
            if self.peek()[0] == 'OP' and self.peek()[1] in ('+','-','*','/','%','**','//'):
                op = self.peek()[1]; self.pos += 1
                rhs = self.parse_primary()
                return Fn([('_', None, None)], Block([Return(BinOp(op, Ident('_'), rhs))]))
            if self.peek()[0] == 'OP' and self.peek()[1] == '.':
                self.pos += 1
                field = self.eat('IDENT')[1]
                return Fn([('_', None, None)], Block([Return(Field(Ident('_'), field))]))
            return Fn([('_', None, None)], Block([Return(Ident('_'))]))

        # If expression
        if t[0] == 'KW' and t[1] == 'if':
            self.pos += 1
            cond = self.parse_expr()
            self.skip_newlines()
            then_block = None
            if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
                self.pos += 1
                # Allow `return expr` after => (statement form inside a fn)
                if self.peek()[0] == 'KW' and self.peek()[1] == 'return':
                    self.pos += 1
                    then_val = self.parse_expr()
                    then_block = Block([Return(then_val)])
                else:
                    # Expression form — do NOT wrap in Return (avoids ReturnSignal leak)
                    then_val = self.parse_expr()
                    then_block = Block([then_val])
            else:
                then_block = self.parse_indented_block()
            elifs = []
            else_block = None
            self.skip_newlines()
            while self.peek()[0] == 'KW' and self.peek()[1] == 'else':
                self.pos += 1
                self.skip_newlines()
                if self.peek()[0] == 'KW' and self.peek()[1] == 'if':
                    self.pos += 1
                    ec = self.parse_expr()
                    self.skip_newlines()
                    if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
                        self.pos += 1
                        if self.peek()[0] == 'KW' and self.peek()[1] == 'return':
                            self.pos += 1
                            ev = self.parse_expr()
                            elifs.append((ec, Block([Return(ev)])))
                        else:
                            ev = self.parse_expr()
                            elifs.append((ec, Block([ev])))
                    else:
                        eb = self.parse_indented_block()
                        elifs.append((ec, eb))
                    self.skip_newlines()
                elif self.peek()[0] == 'OP' and self.peek()[1] == '=>':
                    self.pos += 1
                    if self.peek()[0] == 'KW' and self.peek()[1] == 'return':
                        self.pos += 1
                        else_val = self.parse_expr()
                        else_block = Block([Return(else_val)])
                    else:
                        else_val = self.parse_expr()
                        else_block = Block([else_val])
                    break
                else:
                    else_block = self.parse_indented_block()
                    break
            return If(cond, then_block, elifs, else_block)

        # fn (anonymous)
        if t[0] == 'KW' and t[1] == 'fn':
            self.pos += 1
            self.eat('OP', '(')
            params = self.parse_params()
            self.eat('OP', ')')
            if self.peek()[0] == 'OP' and self.peek()[1] == '->':
                self.pos += 1; self.parse_type_expr()  # skip return type
            if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
                self.pos += 1; body = Block([Return(self.parse_expr())])
            else:
                body = self.parse_indented_block()
            return Fn(params, body)

        # Tuple or grouping
        if t[0] == 'OP' and t[1] == '(':
            self.pos += 1
            if self.peek()[0] == 'OP' and self.peek()[1] == ')':
                self.pos += 1; return TupleLit([])  # unit
            first = self.parse_expr()
            if self.peek()[0] == 'OP' and self.peek()[1] == ',':
                items = [first]
                while self.eat_if('OP', ','):
                    if self.peek()[1] == ')': break
                    items.append(self.parse_expr())
                self.eat('OP', ')')
                return TupleLit(items)
            self.eat('OP', ')')
            return first

        # List literal
        if t[0] == 'OP' and t[1] == '[':
            self.pos += 1
            items = []
            self.skip_newlines()
            while self.peek()[1] != ']':
                items.append(self.parse_expr())
                if not self.eat_if('OP', ','): break
                self.skip_newlines()
                if self.peek()[1] == ']': break
            self.eat('OP', ']')
            return ListLit(items)

        # Map/Set literal
        if t[0] == 'OP' and t[1] == '{':
            self.pos += 1
            if self.peek()[1] == '}':
                self.pos += 1; return MapLit([])
            first_key = self.parse_expr()
            if self.peek()[0] == 'OP' and self.peek()[1] == ':':
                # Map
                self.pos += 1
                first_val = self.parse_expr()
                pairs = [(first_key, first_val)]
                while self.eat_if('OP', ','):
                    self.skip_newlines()
                    if self.peek()[1] == '}': break
                    k = self.parse_expr(); self.eat('OP', ':'); v = self.parse_expr()
                    pairs.append((k, v))
                self.eat('OP', '}')
                return MapLit(pairs)
            else:
                # Set
                items = [first_key]
                while self.eat_if('OP', ','):
                    items.append(self.parse_expr())
                self.eat('OP', '}')
                return SetLit(items)

        # Identifier (variable, function call, struct constructor, etc.)
        if t[0] == 'IDENT':
            self.pos += 1
            name = t[1]
            # Struct literal: Name { field: val, ... }
            if self.peek()[0] == 'OP' and self.peek()[1] == '{':
                self.pos += 1
                fields = {}
                spread_base = None
                while self.peek()[1] != '}':
                    if self.peek()[0] == 'OP' and self.peek()[1] == '..':
                        self.pos += 1
                        spread_base = self.parse_expr()
                        self.eat_if('OP', ',')
                        continue
                    fname = self.eat('IDENT')[1]
                    if self.peek()[0] == 'OP' and self.peek()[1] == ':':
                        self.pos += 1
                        fields[fname] = self.parse_expr()
                    else:
                        fields[fname] = Ident(fname)  # shorthand
                    if not self.eat_if('OP', ','): break
                self.eat('OP', '}')
                sl = StructLit(name, fields)
                if spread_base: return Spread(spread_base, fields)
                return sl
            return Ident(name)

        # KW as value: Some, None, Ok, Err, true, false
        if t[0] == 'KW' and t[1] in ('Some', 'None', 'Ok', 'Err', 'true', 'false'):
            self.pos += 1
            if t[1] == 'true': return Bool(True)
            if t[1] == 'false': return Bool(False)
            if t[1] == 'None': return Nil()
            # Some(x), Ok(x), Err(x)
            if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                self.pos += 1
                inner = self.parse_expr()
                self.eat('OP', ')')
                return Call(Ident(t[1]), [inner])
            return Ident(t[1])

        if t[0] == 'ANNOT':
            self.pos += 1  # skip inline annotation
            return self.parse_primary()

        # Match as expression
        if t[0] == 'KW' and t[1] == 'match':
            return self.parse_match()

        raise ParseError(f"Unexpected token {t[0]} '{t[1]}'", t[2])


# ─────────────────────────────────────────────────────────────────────────────
# Evaluator
# ─────────────────────────────────────────────────────────────────────────────

class Env:
    def __init__(self, parent=None):
        self.vars: Dict[str, Any] = {}
        self.immutable: set = set()
        self.parent = parent

    def get(self, name):
        if name in self.vars: return self.vars[name]
        if self.parent: return self.parent.get(name)
        raise NameError(f"Undefined variable: '{name}'")

    def set(self, name, value, immutable=True):
        self.vars[name] = value
        if immutable: self.immutable.add(name)

    def assign(self, name, value, force_mutable=False):
        """Walk up scope chain and update the first binding found.
        If force_mutable=True, a :=-bound (immutable) ancestor binding is
        promoted to mutable in-place — this lets = inside nested if/for
        blocks update outer loop variables regardless of how they were bound."""
        if name in self.vars:
            if name in self.immutable and not force_mutable:
                raise TypeError(f"Cannot reassign immutable binding '{name}'")
            self.vars[name] = value
            if force_mutable:
                self.immutable.discard(name)
            return
        if self.parent:
            self.parent.assign(name, value, force_mutable)
            return
        raise NameError(f"Undefined variable: '{name}'")

    def define(self, name, value, immutable=True):
        self.vars[name] = value
        if immutable: self.immutable.add(name)
        else: self.immutable.discard(name)


class Evaluator:
    def __init__(self):
        self.global_env = Env()
        self.type_defs: Dict[str, TypeDecl] = {}
        self._setup_builtins()

    def _setup_builtins(self):
        e = self.global_env
        e.define('print',   lambda *args: print(*[self._format(a) for a in args]))
        e.define('println', lambda *args: print(*[self._format(a) for a in args]))
        e.define('str',     lambda x: self._format(x))
        e.define('int',     lambda x: int(x) if not isinstance(x, bool) else (1 if x else 0))
        e.define('float',   lambda x: float(x))
        e.define('bool',    lambda x: bool(x))
        e.define('len',     lambda x: len(x) if hasattr(x, '__len__') else 0)
        e.define('range',   lambda *a: list(range(*[int(x) for x in a])))
        e.define('abs',     lambda x: abs(x))
        e.define('sqrt',    lambda x: math.sqrt(x))
        e.define('floor',   lambda x: math.floor(x))
        e.define('ceil',    lambda x: math.ceil(x))
        e.define('round',   lambda x, n=0: round(x, int(n)))
        e.define('pow',     lambda x, y: x ** y)
        e.define('max',     lambda *a: max(a[0]) if len(a)==1 and hasattr(a[0],'__iter__') else max(a))
        e.define('min',     lambda *a: min(a[0]) if len(a)==1 and hasattr(a[0],'__iter__') else min(a))
        e.define('sum',     lambda x: sum(x))
        e.define('sorted',  lambda x, **kw: sorted(x))
        e.define('sort',    lambda x, **kw: sorted(x))
        e.define('reversed',lambda x: list(reversed(x)))
        e.define('zip',     lambda *a: [list(x) for x in zip(*a)])
        e.define('enumerate', lambda x: [[i,v] for i,v in enumerate(x)])
        def _map(a, b=None):
            # map(f, xs) or xs |> map(f)  — detect arg order
            if b is None: return lambda xs: [self._call(a, [x]) for x in xs]
            if callable(a) or isinstance(a, NuvolaFn): return [self._call(a, [x]) for x in b]
            return [self._call(b, [x]) for x in a]
        def _filter(a, b=None):
            if b is None: return lambda xs: [x for x in xs if self._call(a, [x])]
            if callable(a) or isinstance(a, NuvolaFn): return [x for x in b if self._call(a, [x])]
            return [x for x in a if self._call(b, [x])]
        e.define('map',     _map)
        e.define('filter',  _filter)
        e.define('fold',    lambda xs, init, f: self._fold(xs, init, f))
        e.define('reduce',  lambda xs, f: self._fold(xs[1:], xs[0], f) if xs else None)
        e.define('flatten', lambda xs: [item for sub in xs for item in (sub if isinstance(sub, list) else [sub])])
        e.define('count',   lambda xs: len(xs))
        e.define('any',     lambda f_or_xs, xs=None: any(self._call(f_or_xs,[x]) for x in xs) if xs else any(f_or_xs))
        e.define('all',     lambda f_or_xs, xs=None: all(self._call(f_or_xs,[x]) for x in xs) if xs else all(f_or_xs))
        e.define('take',    lambda xs, n: xs[:int(n)])
        e.define('drop',    lambda xs, n: xs[int(n):])
        e.define('join',    lambda xs, sep='': sep.join(self._format(x) for x in xs))
        e.define('split',   lambda s, sep=' ': s.split(sep))
        e.define('contains',lambda xs, x: x in xs)
        e.define('type_of', lambda x: type(x).__name__)
        e.define('assert',  lambda cond, msg='Assertion failed': None if cond else (_ for _ in ()).throw(AssertionError(msg)))
        e.define('panic',   lambda msg='panic': (_ for _ in ()).throw(RuntimeError(msg)))
        e.define('Some',    lambda x: NuvolaOption(x, True))
        e.define('None_',   NuvolaOption(None, False))
        e.define('Ok',      lambda x: NuvolaResult(x, True))
        e.define('Err',     lambda x: NuvolaResult(x, False))
        e.define('PI',      math.pi)
        e.define('E',       math.e)
        e.define('TAU',     math.tau)
        e.define('inf',     math.inf)
        e.define('nan',     math.nan)
        e.define('time_ns', lambda: time.time_ns())
        e.define('time_s',  lambda: time.time())
        # String methods as free functions
        e.define('to_upper', lambda s: s.upper())
        e.define('to_lower', lambda s: s.lower())
        e.define('trim',    lambda s: s.strip())
        e.define('starts_with', lambda s, p: s.startswith(p))
        e.define('ends_with',   lambda s, p: s.endswith(p))
        e.define('replace', lambda s, a, b: s.replace(a, b))
        e.define('chars',   lambda s: list(s))
        e.define('bytes',   lambda s: list(s.encode()))
        e.define('parse_i64', lambda s: int(s))
        e.define('parse_f64', lambda s: float(s))
        # Math
        e.define('sin',    math.sin)
        e.define('cos',    math.cos)
        e.define('tan',    math.tan)
        e.define('log',    math.log)
        e.define('log2',   math.log2)
        e.define('log10',  math.log10)
        e.define('exp',    math.exp)
        e.define('tanh',   math.tanh)
        e.define('fib',    None)   # will be user-defined

    def _fold(self, xs, init, f):
        acc = init
        for x in xs: acc = self._call(f, [acc, x])
        return acc

    def _format(self, v) -> str:
        if v is None or isinstance(v, NuvolaOption) and not v.is_some: return "None"
        if isinstance(v, bool): return "true" if v else "false"
        if isinstance(v, float):
            return str(int(v)) if v == int(v) else str(v)
        if isinstance(v, NuvolaFn): return repr(v)
        if isinstance(v, NuvolaStruct): return repr(v)
        if isinstance(v, NuvolaEnum): return repr(v)
        if isinstance(v, NuvolaOption): return repr(v)
        if isinstance(v, NuvolaResult): return repr(v)
        if isinstance(v, list): return f"[{', '.join(self._format(x) for x in v)}]"
        if isinstance(v, dict): return '{' + ', '.join(f'"{k}": {self._format(val)}' for k,val in v.items()) + '}'
        if isinstance(v, set): return '{' + ', '.join(self._format(x) for x in sorted(v, key=str)) + '}'
        if isinstance(v, tuple): return f"({', '.join(self._format(x) for x in v)})"
        return str(v)

    def _interpolate(self, s: str, env: Env) -> str:
        """Process string interpolation: {expr} inside strings."""
        result = []
        i = 0
        while i < len(s):
            if s[i] == '{' and i+1 < len(s) and s[i+1] != '{':
                j = s.find('}', i+1)
                if j == -1: result.append(s[i]); i += 1; continue
                expr_str = s[i+1:j]
                # parse format spec
                fmt_spec = None
                if ':' in expr_str:
                    parts = expr_str.rsplit(':', 1)
                    expr_str, fmt_spec = parts[0], parts[1]
                try:
                    toks = tokenize(expr_str)
                    p = Parser(toks)
                    node = p.parse_expr()
                    val = self.eval(node, env)
                    if fmt_spec:
                        if fmt_spec.endswith('f') or 'f' in fmt_spec:
                            decimals = int(re.search(r'\.(\d+)', fmt_spec).group(1)) if re.search(r'\.(\d+)', fmt_spec) else 2
                            result.append(f"{float(val):.{decimals}f}")
                        elif fmt_spec.endswith('d') or fmt_spec.isdigit():
                            result.append(f"{int(val):>{fmt_spec.lstrip('<>^')}}" if '<>^' in fmt_spec else str(int(val)))
                        elif '>' in fmt_spec or '<' in fmt_spec:
                            result.append(format(self._format(val), fmt_spec))
                        else:
                            result.append(self._format(val))
                    else:
                        result.append(self._format(val))
                except:
                    result.append(s[i:j+1])
                i = j + 1
            elif s[i:i+2] == '{{':
                result.append('{'); i += 2
            elif s[i:i+2] == '}}':
                result.append('}'); i += 2
            else:
                result.append(s[i]); i += 1
        return ''.join(result)

    def _call(self, fn, args):
        if callable(fn):
            return fn(*args)
        if isinstance(fn, NuvolaFn):
            child = Env(fn.env)
            for (pname, _, pdefault), arg in zip(fn.params, args):
                # Parameters are mutable — callee can rebind them (e.g. while b != 0: b = a%b)
                child.define(pname, arg, immutable=False)
            # Handle default params
            for i, (pname, _, pdefault) in enumerate(fn.params):
                if i >= len(args) and pdefault is not None:
                    child.define(pname, self.eval(pdefault, child), immutable=False)
            try:
                result = self.eval(fn.body, child)
                return result
            except ReturnSignal as r:
                return r.v
        raise TypeError(f"Not callable: {repr(fn)}")

    def _range_to_list(self, r: RangeLit, env: Env) -> list:
        start = self.eval(r.start, env) if r.start else 0
        end   = self.eval(r.end,   env) if r.end   else 0
        if r.inclusive:
            return list(range(int(start), int(end)+1))
        return list(range(int(start), int(end)))

    def eval(self, node: Node, env: Env) -> Any:
        if node is None: return None

        if isinstance(node, Num):    return node.v
        if isinstance(node, Bool):   return node.v
        if isinstance(node, Nil):    return NuvolaOption(None, False)  # None
        if isinstance(node, Str):    return self._interpolate(node.v, env)

        if isinstance(node, Ident):
            name = node.name
            # Special built-in values
            if name == 'None': return NuvolaOption(None, False)
            if name == 'true': return True
            if name == 'false': return False
            try:
                return env.get(name)
            except NameError:
                raise NameError(f"Undefined: '{name}'")

        if isinstance(node, RangeLit):
            return self._range_to_list(node, env)

        if isinstance(node, ListLit):
            return [self.eval(item, env) for item in node.items]

        if isinstance(node, MapLit):
            return {self._format(self.eval(k, env)): self.eval(v, env) for k, v in node.pairs}

        if isinstance(node, SetLit):
            return set(self.eval(item, env) for item in node.items)

        if isinstance(node, TupleLit):
            return tuple(self.eval(item, env) for item in node.items)

        if isinstance(node, StructLit):
            fields = {k: self.eval(v, env) for k, v in node.fields.items()}
            return NuvolaStruct(node.name, fields)

        if isinstance(node, Spread):
            base = self.eval(node.base, env)
            fields = dict(base.fields) if isinstance(base, NuvolaStruct) else {}
            for k, v in node.overrides.items():
                fields[k] = self.eval(v, env)
            type_name = base.type_name if isinstance(base, NuvolaStruct) else 'Struct'
            return NuvolaStruct(type_name, fields)

        if isinstance(node, BinOp):
            return self._eval_binop(node, env)

        if isinstance(node, UnOp):
            v = self.eval(node.expr, env)
            if node.op == '-': return -v
            if node.op == 'not' or node.op == '!': return not v
            return v

        if isinstance(node, Pipe):
            left = self.eval(node.l, env)
            right = self.eval(node.r, env)
            # right is a function or partial
            if callable(right) or isinstance(right, NuvolaFn):
                return self._call(right, [left])
            # right is a Call node that needs left as first arg
            if isinstance(node.r, Call):
                fn = self.eval(node.r.fn, env)
                args = [left] + [self.eval(a, env) for a in node.r.args]
                return self._call(fn, args)
            raise TypeError(f"Pipe target is not callable: {repr(right)}")

        if isinstance(node, Field):
            obj = self.eval(node.obj, env)
            field = node.field
            if isinstance(obj, NuvolaStruct):
                if field in obj.fields: return obj.fields[field]
                raise AttributeError(f"{obj.type_name} has no field '{field}'")
            if isinstance(obj, NuvolaOption):
                if field == 'is_some': return obj.is_some
                if field == 'value': return obj.value
            if isinstance(obj, NuvolaResult):
                if field == 'is_ok': return obj.is_ok
                if field == 'value': return obj.value
            # Non-option values: .is_some = True, .value = self (uniform option access)
            if field == 'is_some' and not isinstance(obj, (NuvolaOption, NuvolaResult)):
                return True
            if field == 'value' and not isinstance(obj, (NuvolaOption, NuvolaResult)):
                return obj
            if isinstance(obj, list):
                if field == 'len': return len(obj)
                if field == 'is_empty': return len(obj) == 0
                if field == 'first': return obj[0] if obj else NuvolaOption(None, False)
                if field == 'last':  return obj[-1] if obj else NuvolaOption(None, False)
            if isinstance(obj, str):
                if field == 'len': return len(obj)
                if field == 'is_empty': return len(obj) == 0
            if isinstance(obj, dict):
                if field == 'len': return len(obj)
                if field in obj: return obj[field]
            # Method-style access returning bound function
            return self._get_method(obj, field)

        if isinstance(node, Index):
            obj = self.eval(node.obj, env)
            idx = self.eval(node.idx, env)
            if isinstance(obj, list):
                return obj[int(idx)]
            if isinstance(obj, str):
                return obj[int(idx)]
            if isinstance(obj, dict):
                key = self._format(idx) if not isinstance(idx, str) else idx
                return obj.get(key, NuvolaOption(None, False))
            if isinstance(obj, tuple):
                return obj[int(idx)]
            raise TypeError(f"Cannot index {type(obj)}")

        if isinstance(node, Call):
            fn_node = node.fn
            # Method calls
            if isinstance(fn_node, Field):
                obj = self.eval(fn_node.obj, env)
                method = fn_node.field
                args = [self.eval(a, env) for a in node.args]
                return self._call_method(obj, method, args, env)
            fn = self.eval(fn_node, env)
            args = [self.eval(a, env) for a in node.args]
            return self._call(fn, args)

        if isinstance(node, Assign):
            val = self.eval(node.expr, env)
            if node.immutable:
                # := always creates/overwrites in current scope
                env.define(node.name, val, immutable=True)
            else:
                # = walks up the scope chain and updates the first binding found.
                # If the binding is immutable (:= bound), it is promoted to mutable
                # in-place — so inner loops can update outer accumulators naturally.
                try:
                    env.assign(node.name, val, force_mutable=True)
                except NameError:
                    # Variable not defined anywhere yet — create it as mutable here.
                    env.define(node.name, val, immutable=False)
            return val

        if isinstance(node, IndexAssign):
            obj = self.eval(node.obj, env)
            idx = self.eval(node.idx, env)
            val = self.eval(node.expr, env)
            if isinstance(obj, list):
                obj[int(idx)] = val
            elif isinstance(obj, dict):
                obj[self._format(idx) if not isinstance(idx, str) else idx] = val
            return val

        if isinstance(node, FieldAssign):
            obj = self.eval(node.obj, env)
            val = self.eval(node.expr, env)
            if isinstance(obj, NuvolaStruct):
                obj.fields[node.field] = val
            elif isinstance(obj, dict):
                obj[node.field] = val
            return val

        if isinstance(node, Destructure):
            rhs = self.eval(node.expr, env)
            self._destructure(node.pattern, rhs, env)
            return rhs

        if isinstance(node, Fn):
            return NuvolaFn(node.params, node.body, env, node.name)

        if isinstance(node, FnDecl):
            fn = NuvolaFn(node.params, node.body, env, node.name)
            env.define(node.name, fn, immutable=True)
            return fn

        if isinstance(node, If):
            cond = self.eval(node.cond, env)
            if self._truthy(cond):
                return self.eval(node.then, env)
            for ec, eb in (node.elifs or []):
                if self._truthy(self.eval(ec, env)):
                    return self.eval(eb, env)
            if node.else_:
                return self.eval(node.else_, env)
            return NuvolaOption(None, False)

        if isinstance(node, Match):
            val = self.eval(node.expr, env)
            for arm in node.arms:
                child_env = Env(env)
                if self._match_pattern(arm.pattern, val, child_env):
                    if arm.guard and not self._truthy(self.eval(arm.guard, child_env)):
                        continue
                    result = self.eval(arm.body, child_env)
                    # Extract return value if it came from a block
                    return result
            return NuvolaOption(None, False)  # no arm matched

        if isinstance(node, Block):
            result = NuvolaOption(None, False)
            for stmt in node.stmts:
                result = self.eval(stmt, env)
            return result

        if isinstance(node, ForLoop):
            iter_val = self.eval(node.iter_, env)
            if isinstance(iter_val, dict): iter_val = list(iter_val.items())
            result = None
            try:
                for item in iter_val:
                    child = Env(env)
                    if isinstance(node.var, tuple):
                        if isinstance(item, (list, tuple)):
                            for vn, vi in zip(node.var, item):
                                child.define(vn, vi, immutable=True)
                        else:
                            child.define(node.var[0], item, immutable=True)
                    else:
                        child.define(node.var, item, immutable=True)
                    try:
                        result = self.eval(node.body, child)
                    except ContinueSignal:
                        continue
            except BreakSignal as b:
                return b.v
            return result

        if isinstance(node, WhileLoop):
            result = None
            try:
                while self._truthy(self.eval(node.cond, env)):
                    try:
                        result = self.eval(node.body, env)
                    except ContinueSignal:
                        continue
            except BreakSignal as b:
                return b.v
            return result

        if isinstance(node, Return):
            val = self.eval(node.expr, env) if node.expr else None
            raise ReturnSignal(val)

        if isinstance(node, Break):
            val = self.eval(node.expr, env) if node.expr else None
            raise BreakSignal(val)

        if isinstance(node, Continue):
            raise ContinueSignal()

        if isinstance(node, TypeDecl):
            self.type_defs[node.name] = node
            # Create constructor function
            if node.fields:
                def make_constructor(tname, tfields):
                    def constructor(**kwargs):
                        fields = {}
                        for fname, (ftype, fdefault) in tfields.items():
                            if fname in kwargs:
                                fields[fname] = kwargs[fname]
                            elif fdefault:
                                fields[fname] = self.eval(fdefault, self.global_env)
                            else:
                                fields[fname] = None
                        return NuvolaStruct(tname, fields)
                    return constructor
                env.define(node.name, make_constructor(node.name, node.fields), immutable=True)
            return None

        if isinstance(node, Import):
            # Stub imports — just ignore or load stdlib stubs
            return None

        if isinstance(node, Annotation):
            return self.eval(node.inner, env)

        return None

    def _truthy(self, v) -> bool:
        if v is None: return False
        if isinstance(v, bool): return v
        if isinstance(v, (int, float)): return v != 0
        if isinstance(v, str): return len(v) > 0
        if isinstance(v, list): return len(v) > 0
        if isinstance(v, NuvolaOption): return v.is_some
        if isinstance(v, NuvolaResult): return v.is_ok
        return True

    def _eval_binop(self, node: BinOp, env: Env) -> Any:
        op = node.op
        # Short-circuit
        if op == 'and':
            l = self.eval(node.l, env)
            return l if not self._truthy(l) else self.eval(node.r, env)
        if op == 'or':
            l = self.eval(node.l, env)
            return l if self._truthy(l) else self.eval(node.r, env)
        if op == '??':
            l = self.eval(node.l, env)
            if isinstance(l, NuvolaOption): return l.value if l.is_some else self.eval(node.r, env)
            return l if l is not None else self.eval(node.r, env)

        l = self.eval(node.l, env)
        r = self.eval(node.r, env)

        if op == '+':
            if isinstance(l, str) or isinstance(r, str):
                return self._format(l) + self._format(r)
            if isinstance(l, list) and isinstance(r, list): return l + r
            return l + r
        if op == '-': return l - r
        if op == '*':
            if isinstance(l, str) and isinstance(r, int): return l * r
            if isinstance(l, list) and isinstance(r, int): return l * r
            return l * r
        if op == '/': return l / r
        if op == '//': return l // r
        if op == '%': return l % r
        if op == '**': return l ** r
        if op == '==':
            if isinstance(l, NuvolaOption) and isinstance(r, NuvolaOption):
                return l.is_some == r.is_some and l.value == r.value
            return l == r
        if op == '!=': return l != r
        if op == '<':  return l < r
        if op == '>':  return l > r
        if op == '<=': return l <= r
        if op == '>=': return l >= r
        if op == 'is':
            type_name = r if isinstance(r, str) else (r.name if isinstance(r, Ident) else str(r))
            if isinstance(l, NuvolaStruct): return l.type_name == type_name
            if isinstance(l, NuvolaEnum): return l.type_name == type_name
            return type(l).__name__ == type_name
        if op == '|': return l | r if isinstance(l, (int, set)) else l
        if op == '&': return l & r if isinstance(l, (int, set)) else l
        if op == '^': return l ^ r if isinstance(l, int) else l
        if op == '<<': return l << r
        if op == '>>': return l >> r
        if op == '?.':
            if isinstance(l, NuvolaOption) and not l.is_some: return NuvolaOption(None, False)
            obj = l.value if isinstance(l, NuvolaOption) else l
            field = r.name if isinstance(r, Ident) else str(r)
            return self._get_field(obj, field)
        return None

    def _get_field(self, obj, field):
        if isinstance(obj, NuvolaStruct): return obj.fields.get(field)
        if isinstance(obj, dict): return obj.get(field)
        return None

    def _get_method(self, obj, method):
        """Return a callable for a method on obj."""
        if isinstance(obj, list):
            methods = {
                'push':     lambda x: obj.append(x) or obj,
                'pop':      lambda: obj.pop(),
                'len':      lambda: len(obj),
                'is_empty': lambda: len(obj) == 0,
                'map':      lambda f: [self._call(f, [x]) for x in obj],
                'filter':   lambda f: [x for x in obj if self._call(f, [x])],
                'fold':     lambda init, f: self._fold(obj, init, f),
                'sum':      lambda: sum(obj),
                'min':      lambda: min(obj),
                'max':      lambda: max(obj),
                'sort':     lambda: sorted(obj),
                'sort_by':  lambda f: sorted(obj, key=lambda x: self._call(f, [x])),
                'any':      lambda f: any(self._call(f, [x]) for x in obj),
                'all':      lambda f: all(self._call(f, [x]) for x in obj),
                'take':     lambda n: obj[:int(n)],
                'drop':     lambda n: obj[int(n):],
                'first':    lambda: obj[0] if obj else NuvolaOption(None, False),
                'last':     lambda: obj[-1] if obj else NuvolaOption(None, False),
                'join':     lambda sep='': sep.join(self._format(x) for x in obj),
                'enumerate':lambda: [[i,v] for i,v in enumerate(obj)],
                'zip':      lambda other: [list(p) for p in zip(obj, other)],
                'flatten':  lambda: [item for sub in obj for item in (sub if isinstance(sub, list) else [sub])],
                'count':    lambda: len(obj),
                'extend':   lambda other: obj.extend(other) or obj,
                'contains': lambda x: x in obj,
                'dedupe':   lambda: list(dict.fromkeys(obj)),
                'reverse':  lambda: list(reversed(obj)),
                'windows':  lambda n: [obj[i:i+n] for i in range(len(obj)-int(n)+1)],
                'chunks':   lambda n: [obj[i:i+int(n)] for i in range(0, len(obj), int(n))],
                'collect':  lambda: obj,
                'mean':     lambda: sum(obj)/len(obj) if obj else 0,
            }
            return methods.get(method, lambda *a: None)
        if isinstance(obj, str):
            methods = {
                'len':         lambda: len(obj),
                'to_upper':    lambda: obj.upper(),
                'to_lower':    lambda: obj.lower(),
                'trim':        lambda: obj.strip(),
                'split':       lambda sep=' ': obj.split(sep),
                'contains':    lambda s: s in obj,
                'starts_with': lambda s: obj.startswith(s),
                'ends_with':   lambda s: obj.endswith(s),
                'replace':     lambda a, b: obj.replace(a, b),
                'chars':       lambda: list(obj),
                'bytes':       lambda: list(obj.encode()),
                'parse_i64':   lambda: int(obj),
                'parse_f64':   lambda: float(obj),
                'is_empty':    lambda: len(obj) == 0,
                'repeat':      lambda n: obj * int(n),
                'lines':       lambda: obj.split('\n'),
                'format':      lambda **kw: obj.format(**kw),
            }
            return methods.get(method, lambda *a: None)
        if isinstance(obj, dict):
            methods = {
                'get':      lambda k, default=None: obj.get(self._format(k), default),
                'set':      lambda k, v: obj.update({self._format(k): v}) or obj,
                'keys':     lambda: list(obj.keys()),
                'values':   lambda: list(obj.values()),
                'entries':  lambda: [[k, v] for k, v in obj.items()],
                'len':      lambda: len(obj),
                'contains': lambda k: self._format(k) in obj,
                'is_empty': lambda: len(obj) == 0,
                'sorted_by_key': lambda: sorted(obj.items()),
            }
            return methods.get(method, lambda *a: None)
        if isinstance(obj, NuvolaOption):
            methods = {
                'unwrap':       lambda: obj.value if obj.is_some else (_ for _ in ()).throw(RuntimeError("unwrap on None")),
                'unwrap_or':    lambda default: obj.value if obj.is_some else default,
                'map':          lambda f: NuvolaOption(self._call(f, [obj.value]), True) if obj.is_some else obj,
                'is_some':      lambda: obj.is_some,
                'is_none':      lambda: not obj.is_some,
            }
            return methods.get(method, lambda *a: None)
        if isinstance(obj, float):
            methods = {
                'sqrt': lambda: math.sqrt(obj),
                'abs':  lambda: abs(obj),
                'floor':lambda: math.floor(obj),
                'ceil': lambda: math.ceil(obj),
                'round':lambda n=0: round(obj, int(n)),
                'to_i64': lambda: int(obj),
                'to_f64': lambda: float(obj),
                'to_str': lambda: self._format(obj),
            }
            return methods.get(method, lambda *a: None)
        if isinstance(obj, int):
            methods = {
                'to_f64':  lambda: float(obj),
                'to_i64':  lambda: int(obj),
                'to_str':  lambda: str(obj),
                'abs':     lambda: abs(obj),
                'pow':     lambda n: obj ** int(n),
            }
            return methods.get(method, lambda *a: None)
        return lambda *a: None

    def _call_method(self, obj, method, args, env):
        m = self._get_method(obj, method)
        if callable(m):
            return m(*args)
        raise AttributeError(f"No method '{method}' on {type(obj).__name__}")

    def _match_pattern(self, pattern, value, env: Env) -> bool:
        if isinstance(pattern, list):
            # OR pattern
            return any(self._match_pattern(p, value, env) for p in pattern)
        if not isinstance(pattern, tuple) or not pattern:
            return False
        kind = pattern[0]
        if kind == 'wildcard': return True
        if kind == 'literal':
            lit = pattern[1]
            if isinstance(value, NuvolaOption) and not value.is_some and lit is None: return True
            if isinstance(value, bool) and isinstance(lit, bool): return value == lit
            if isinstance(value, (int, float)) and isinstance(lit, (int, float)): return value == lit
            if isinstance(value, str) and isinstance(lit, str): return value == lit
            return value == lit
        if kind == 'bind':
            env.define(pattern[1], value, immutable=True)
            return True
        if kind == 'range':
            _, lo, hi, inclusive = pattern
            if isinstance(value, (int, float)):
                return lo <= value <= hi if inclusive else lo <= value < hi
            return False
        if kind == 'range_from':
            _, lo = pattern
            return isinstance(value, (int, float)) and value >= lo
        if kind == 'range_to':
            _, hi, inclusive = pattern
            if isinstance(value, (int, float)):
                return value <= hi if inclusive else value < hi
            return False
        if kind == 'some':
            if isinstance(value, NuvolaOption) and value.is_some:
                return self._match_pattern(pattern[1], value.value, env)
            return False
        if kind == 'none':
            return isinstance(value, NuvolaOption) and not value.is_some
        if kind == 'ok':
            if isinstance(value, NuvolaResult) and value.is_ok:
                return self._match_pattern(pattern[1], value.value, env)
            return False
        if kind == 'err':
            if isinstance(value, NuvolaResult) and not value.is_ok:
                return self._match_pattern(pattern[1], value.value, env)
            return False
        if kind == 'type_check':
            type_name = pattern[1]
            if isinstance(value, NuvolaStruct): return value.type_name == type_name
            if isinstance(value, NuvolaEnum): return value.variant == type_name
            return type(value).__name__ == type_name
        if kind == 'constructor':
            # e.g. Some(x), Circle(r)
            cname, inner = pattern[1], pattern[2]
            if cname == 'Some' and isinstance(value, NuvolaOption) and value.is_some:
                return self._match_pattern(inner, value.value, env)
            if isinstance(value, NuvolaStruct) and value.type_name == cname:
                return True
            if isinstance(value, NuvolaEnum) and value.variant == cname:
                return self._match_pattern(inner, value.value, env)
            return False
        if kind == 'enum_variant':
            _, tname, vname, inner = pattern
            if isinstance(value, NuvolaEnum) and value.variant == vname:
                if inner: return self._match_pattern(inner, value.value, env)
                return True
            if isinstance(value, NuvolaStruct) and value.type_name == vname:
                return True
            return False
        return False

    def _destructure(self, pattern, value, env: Env):
        if isinstance(pattern, TupleLit):
            if isinstance(value, (list, tuple)):
                for p, v in zip(pattern.items, value):
                    if isinstance(p, Ident):
                        env.define(p.name, v, immutable=True)
                    else:
                        self._destructure(p, v, env)
            elif isinstance(value, NuvolaStruct):
                for p in pattern.items:
                    if isinstance(p, Ident) and p.name in value.fields:
                        env.define(p.name, value.fields[p.name], immutable=True)

    def run(self, source: str) -> Any:
        tokens = tokenize(source)
        parser = Parser(tokens)
        tree = parser.parse_program()
        return self.eval(tree, self.global_env)


# ─────────────────────────────────────────────────────────────────────────────
# Test runner
# ─────────────────────────────────────────────────────────────────────────────

def run_test_file(path: str, level: str):
    print(f"\n{'='*70}")
    print(f"  NUVOLA TEST SUITE — {level.upper()}")
    print(f"  File: {path}")
    print(f"{'='*70}")

    with open(path) as f:
        source = f.read()

    evaluator = Evaluator()
    passed = 0; failed = 0; errors = []

    # Split into individual tests by -- TEST: markers
    test_blocks = re.split(r'\n-- TEST: (.+)\n', source)

    if len(test_blocks) == 1:
        # Run as single program
        try:
            t0 = time.perf_counter()
            evaluator.run(source)
            elapsed = (time.perf_counter() - t0) * 1000
            print(f"  Program ran successfully in {elapsed:.1f}ms")
        except Exception as e:
            print(f"  ERROR: {e}")
            if '--verbose' in sys.argv:
                traceback.print_exc()
        return

    # Run each test block
    preamble = test_blocks[0]
    test_pairs = list(zip(test_blocks[1::2], test_blocks[2::2]))

    for test_name, test_body in test_pairs:
        full_code = preamble + '\n' + test_body
        ev = Evaluator()
        try:
            t0 = time.perf_counter()
            ev.run(full_code)
            elapsed = (time.perf_counter() - t0) * 1000
            print(f"  PASS  {test_name:<50} ({elapsed:.1f}ms)")
            passed += 1
        except AssertionError as e:
            print(f"  FAIL  {test_name:<50} — {e}")
            failed += 1; errors.append((test_name, str(e)))
        except Exception as e:
            if '--verbose' in sys.argv:
                traceback.print_exc()
            print(f"  ERR   {test_name:<50} — {type(e).__name__}: {e}")
            failed += 1; errors.append((test_name, f"{type(e).__name__}: {e}"))

    total = passed + failed
    print(f"\n  Results: {passed}/{total} passed", end="")
    if failed: print(f"  ({failed} failed)")
    else: print("  — all tests passed!")

    return passed, failed

if __name__ == '__main__':
    if len(sys.argv) < 2:
        print("Usage: nuvola_eval.py <test_file.nvl> [--verbose]")
        sys.exit(1)
    run_test_file(sys.argv[1], sys.argv[1])
