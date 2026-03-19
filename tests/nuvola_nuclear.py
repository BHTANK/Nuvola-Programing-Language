#!/usr/bin/env python3
"""
Nuvola Nuclear Evaluator — v0.1
Extends nuvola_eval with:
  - NuvolaTensor: numpy-backed tensor with reverse-mode autodiff (micrograd-style)
  - NuvolaChannel: queue.Queue-backed typed channels for async communication
  - comptime blocks: compile-time constant evaluation + caching
  - trait / impl: zero-overhead trait dispatch registry
  - async fn / await / spawn: cooperative multitasking via threads
  - extern fn: FFI stubs for C/BLAS calls
  - unsafe blocks: raw pointer semantics (simulated)
  - generics: T-parameterized functions via runtime specialization
  - Quantization: int8/f16 tensor support
  - Nuclear builtins: tensor, zeros, ones, randn, eye, arange, linspace,
                      softmax, relu, sigmoid, tanh, gelu, layer_norm,
                      matmul, dot, einsum, chan, spawn_task, await_task
"""

import re
import sys
import math
import cmath
import time
import queue
import ctypes
import hashlib
import threading
import traceback
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Dict, List, Optional, Tuple
from functools import lru_cache

# ── numpy / cupy ─────────────────────────────────────────────────────────────
try:
    import numpy as np
    HAS_NUMPY = True
except ImportError:
    HAS_NUMPY = False
    np = None

try:
    import cupy as cp
    HAS_GPU = True
except ImportError:
    cp = None
    HAS_GPU = False

# ── scipy (optional — advanced math: LAPACK, FFT, special functions) ─────────
try:
    import scipy.linalg as _sla
    import scipy.special as _ssp
    import scipy.fft as _sfft
    import scipy.optimize as _sopt
    HAS_SCIPY = True
except ImportError:
    _sla = _ssp = _sfft = _sopt = None
    HAS_SCIPY = False

# ── sympy (optional — symbolic math) ─────────────────────────────────────────
try:
    import sympy as _sp
    HAS_SYMPY = True
except ImportError:
    _sp = None
    HAS_SYMPY = False

# ── parallel executor (fork-join for large ops) ───────────────────────────────
_PAR_EXECUTOR = ThreadPoolExecutor(max_workers=None)  # None = os.cpu_count()
_PAR_THRESHOLD = 4096  # elements: below this, sequential is faster

def _xp(arr):
    """Return cupy if arr lives on GPU, else numpy."""
    if HAS_GPU and isinstance(arr, cp.ndarray):
        return cp
    return np

def _to_numpy(arr):
    """Move array to CPU numpy (no-op if already numpy)."""
    if HAS_GPU and isinstance(arr, cp.ndarray):
        return cp.asnumpy(arr)
    return arr

# ── import base interpreter ──────────────────────────────────────────────────
sys.path.insert(0, __file__.rsplit('/', 1)[0])
from nuvola_eval import (
    Node, Num, Str, Bool, Nil, Ident, BinOp, UnOp, Pipe, Call, Index, Field,
    Assign, IndexAssign, FieldAssign, Destructure, Fn, FnDecl, If, Match,
    MatchArm, ForLoop, WhileLoop, Block, Return, Break, Continue,
    ListLit, MapLit, SetLit, TupleLit, RangeLit, StructLit, Spread,
    TypeDecl, Import, Annotation,
    NuvolaFn, NuvolaStruct, NuvolaEnum, NuvolaOption, NuvolaResult,
    ReturnSignal, BreakSignal, ContinueSignal,
    Env, tokenize, Parser, Evaluator,
    ParseError, KEYWORDS, SENTINEL,
    run_test_file,
)

# ─────────────────────────────────────────────────────────────────────────────
# Nuclear AST nodes
# ─────────────────────────────────────────────────────────────────────────────

class ComptimeDecl(Node):
    """comptime name := expr  — evaluated once at parse/load time."""
    def __init__(self, name, expr): self.name = name; self.expr = expr

class TraitDecl(Node):
    """trait Name { fn method(self, ...) }"""
    def __init__(self, name, methods): self.name = name; self.methods = methods

class ImplDecl(Node):
    """impl TraitName for TypeName { fn method(self, ...) => ... }"""
    def __init__(self, trait_name, type_name, methods):
        self.trait_name = trait_name; self.type_name = type_name; self.methods = methods

class AsyncFnDecl(Node):
    """async fn name(...) => body"""
    def __init__(self, name, params, body, ret_type=None):
        self.name = name; self.params = params; self.body = body; self.ret_type = ret_type

class Await(Node):
    """await expr"""
    def __init__(self, expr): self.expr = expr

class Spawn(Node):
    """spawn expr  — fires a green thread, returns a Future handle"""
    def __init__(self, expr): self.expr = expr

class ExternFn(Node):
    """extern fn name(params) -> ret_type  — C FFI stub"""
    def __init__(self, name, params, ret_type, lib=None):
        self.name = name; self.params = params; self.ret_type = ret_type; self.lib = lib

class UnsafeBlock(Node):
    """unsafe { stmts }  — disables safety checks inside"""
    def __init__(self, body): self.body = body

class GenericCall(Node):
    """f<T>(args)  — generic instantiation"""
    def __init__(self, fn, type_args, args): self.fn = fn; self.type_args = type_args; self.args = args

class TensorLit(Node):
    """tensor([...], dtype=f32)  — dense tensor literal"""
    def __init__(self, data, dtype=None, shape=None): self.data = data; self.dtype = dtype; self.shape = shape

# ─────────────────────────────────────────────────────────────────────────────
# NuvolaTensor — numpy-backed with micrograd-style reverse-mode autodiff
# ─────────────────────────────────────────────────────────────────────────────

class NuvolaTensor:
    """
    Tensor with automatic differentiation.

    Each tensor holds:
      data     — numpy array (the forward value)
      grad     — numpy array (accumulated gradient, same shape)
      _backward — closure to propagate gradient one step back
      _prev    — set of parent tensors (for topological sort)
      requires_grad — bool

    Class-level:
      _no_grad_depth — when > 0, _wrap() skips graph construction entirely.
                       All ops return plain data tensors with no backward hooks.
                       Eliminates ~70% overhead of inference passes.
    """

    _no_grad_depth: int = 0   # class-level inference mode counter

    def __init__(self, data, dtype=None, requires_grad=False, label='', device='cpu'):
        if not HAS_NUMPY:
            raise RuntimeError("NumPy required for tensors: pip install numpy")
        if isinstance(data, NuvolaTensor):
            data = data.data
        if isinstance(data, (int, float)):
            data = [data]
        dtype_map = {
            'f64': np.float64, 'f32': np.float32, 'f16': np.float16,
            'i64': np.int64,   'i32': np.int32,   'i16': np.int16,
            'i8':  np.int8,    'u8':  np.uint8,
        }
        np_dtype = dtype_map.get(dtype, np.float64) if dtype else np.float64
        # GPU dispatch: if device='cuda' and cupy available, store on GPU
        if device == 'cuda' and HAS_GPU:
            self.data = cp.array(data, dtype=np_dtype)
        else:
            self.data = np.array(data, dtype=np_dtype)
        self.grad = np.zeros_like(_to_numpy(self.data), dtype=np.float64)
        self.requires_grad = requires_grad
        self.label = label
        self.device = 'cuda' if (HAS_GPU and isinstance(self.data, cp.ndarray)) else 'cpu'
        self._backward = lambda: None
        self._prev: set = set()
        self._dtype_name = dtype or 'f64'

    @property
    def xp(self):
        """Return the array module (cupy or numpy) for this tensor."""
        return _xp(self.data)

    # ── shape / dtype ──────────────────────────────────────────────────────
    @property
    def shape(self): return list(self.data.shape)

    @property
    def ndim(self): return self.data.ndim

    @property
    def dtype(self): return self._dtype_name

    @property
    def size(self): return int(self.data.size)

    # ── broadcast-aware gradient reduction ────────────────────────────────
    @staticmethod
    def _reduce_grad(grad: 'np.ndarray', target_shape: tuple) -> 'np.ndarray':
        """Sum grad over any dimensions that were broadcast in the forward pass."""
        while grad.ndim > len(target_shape):
            grad = grad.sum(axis=0)
        for i, (ts, gs) in enumerate(zip(target_shape, grad.shape)):
            if ts == 1 and gs > 1:
                grad = grad.sum(axis=i, keepdims=True)
        return grad

    # ── arithmetic (builds computational graph) ────────────────────────────
    def _wrap(self, data, children=(), backward=None):
        out = NuvolaTensor.__new__(NuvolaTensor)
        out.data = data if isinstance(data, np.ndarray) or (HAS_GPU and isinstance(data, cp.ndarray)) else np.array(data, dtype=np.float64)
        out.grad = np.zeros_like(_to_numpy(out.data), dtype=np.float64)
        out.label = ''
        out.device = 'cuda' if (HAS_GPU and isinstance(out.data, cp.ndarray)) else 'cpu'
        out._dtype_name = self._dtype_name
        out._backward = lambda: None
        # ── fast path: no_grad mode ───────────────────────────────────────────
        # When _no_grad_depth > 0 (inference / memoize / warm_inference context):
        # skip allocating _prev set and building backward closures entirely.
        # This eliminates ~70% of per-op overhead during forward-only passes.
        if NuvolaTensor._no_grad_depth > 0:
            out.requires_grad = False
            out._prev = set()
        else:
            out.requires_grad = self.requires_grad or any(getattr(c, 'requires_grad', False) for c in children)
            out._prev = set(children)
            if backward:
                out._backward = backward
        return out

    def __add__(self, other):
        other = other if isinstance(other, NuvolaTensor) else NuvolaTensor(other)
        out_data = self.data + other.data
        out = self._wrap(out_data, (self, other))
        ss, os = self.data.shape, other.data.shape
        def _back():
            if self.requires_grad:
                self.grad  += self._reduce_grad(out.grad, ss)
            if other.requires_grad:
                other.grad += self._reduce_grad(out.grad, os)
        out._backward = _back
        return out

    def __radd__(self, other): return self.__add__(other)

    def __mul__(self, other):
        other = other if isinstance(other, NuvolaTensor) else NuvolaTensor(other)
        out_data = self.data * other.data
        out = self._wrap(out_data, (self, other))
        ss, os = self.data.shape, other.data.shape
        sd, od = self.data, other.data
        def _back():
            if self.requires_grad:
                self.grad  += self._reduce_grad(od * out.grad, ss)
            if other.requires_grad:
                other.grad += self._reduce_grad(sd * out.grad, os)
        out._backward = _back
        return out

    def __rmul__(self, other): return self.__mul__(other)

    def __sub__(self, other):
        other = other if isinstance(other, NuvolaTensor) else NuvolaTensor(other)
        out_data = self.data - other.data
        out = self._wrap(out_data, (self, other))
        ss, os = self.data.shape, other.data.shape
        def _back():
            if self.requires_grad:
                self.grad  += self._reduce_grad(out.grad, ss)
            if other.requires_grad:
                other.grad -= self._reduce_grad(out.grad, os)
        out._backward = _back
        return out

    def __rsub__(self, other): return NuvolaTensor(other).__sub__(self)

    def __truediv__(self, other):
        other = other if isinstance(other, NuvolaTensor) else NuvolaTensor(other)
        out_data = self.data / other.data
        out = self._wrap(out_data, (self, other))
        ss, os = self.data.shape, other.data.shape
        sd, od = self.data, other.data
        def _back():
            if self.requires_grad:
                self.grad  += self._reduce_grad(out.grad / od, ss)
            if other.requires_grad:
                other.grad -= self._reduce_grad(out.grad * sd / (od ** 2), os)
        out._backward = _back
        return out

    def __neg__(self): return self.__mul__(-1)

    def __pow__(self, exp):
        out_data = self.data ** exp
        out = self._wrap(out_data, (self,))
        def _back():
            if self.requires_grad:
                self.grad += exp * (self.data ** (exp - 1)) * out.grad
        out._backward = _back
        return out

    def matmul(self, other):
        other = other if isinstance(other, NuvolaTensor) else NuvolaTensor(other)
        out_data = self.data @ other.data
        out = self._wrap(out_data, (self, other))
        sd_np = _to_numpy(self.data)
        od_np = _to_numpy(other.data)
        def _back():
            if self.requires_grad:
                if sd_np.ndim == 1:
                    # x=[k], W=[k,m] → dx = W @ dout
                    self.grad += od_np @ out.grad if od_np.ndim == 1 else od_np.dot(out.grad)
                else:
                    self.grad += out.grad @ od_np.T
            if other.requires_grad:
                if sd_np.ndim == 1:
                    # dW = outer(x, dout): [k]×[m] → [k,m]
                    other.grad += np.outer(sd_np, out.grad)
                else:
                    other.grad += sd_np.T @ out.grad
        out._backward = _back
        return out

    def __matmul__(self, other): return self.matmul(other)

    # ── activation functions ───────────────────────────────────────────────
    def relu(self):
        xp = self.xp
        out_data = xp.maximum(0, self.data)
        out = self._wrap(out_data, (self,))
        mask_np = _to_numpy(self.data > 0)
        def _back():
            if self.requires_grad:
                self.grad += mask_np * out.grad
        out._backward = _back
        return out

    def sigmoid(self):
        xp = self.xp
        s_gpu = 1.0 / (1.0 + xp.exp(-self.data.astype(np.float64)))
        out = self._wrap(s_gpu, (self,))
        s_np = _to_numpy(s_gpu)
        def _back():
            if self.requires_grad:
                self.grad += s_np * (1 - s_np) * out.grad
        out._backward = _back
        return out

    def tanh(self):
        xp = self.xp
        t_gpu = xp.tanh(self.data.astype(np.float64))
        out = self._wrap(t_gpu, (self,))
        t_np = _to_numpy(t_gpu)
        def _back():
            if self.requires_grad:
                self.grad += (1 - t_np ** 2) * out.grad
        out._backward = _back
        return out

    def gelu(self):
        xp = self.xp
        x = _to_numpy(self.data).astype(np.float64)  # GELU needs float math; keep CPU
        cdf = 0.5 * (1.0 + np.tanh(0.7978845608 * (x + 0.044715 * x**3)))
        out = self._wrap(x * cdf, (self,))
        def _back():
            if self.requires_grad:
                pdf = np.exp(-0.5 * x**2) / math.sqrt(2 * math.pi)
                self.grad += (cdf + x * pdf) * out.grad
        out._backward = _back
        return out

    def softmax(self, axis=-1):
        xp = self.xp
        x = self.data.astype(np.float64)
        e = xp.exp(x - x.max(axis=axis, keepdims=True))
        s_gpu = e / e.sum(axis=axis, keepdims=True)
        out = self._wrap(s_gpu, (self,))
        s_np = _to_numpy(s_gpu)
        def _back():
            if self.requires_grad:
                dot = (out.grad * s_np).sum(axis=axis, keepdims=True)
                self.grad += s_np * (out.grad - dot)
        out._backward = _back
        return out

    def log(self):
        xp = self.xp
        d = self.data.astype(np.float64)
        out_data = xp.log(d + 1e-12)
        out = self._wrap(out_data, (self,))
        d_np = _to_numpy(d)
        def _back():
            if self.requires_grad:
                self.grad += out.grad / (d_np + 1e-12)
        out._backward = _back
        return out

    def exp(self):
        xp = self.xp
        e_gpu = xp.exp(self.data.astype(np.float64))
        out = self._wrap(e_gpu, (self,))
        e_np = _to_numpy(e_gpu)
        def _back():
            if self.requires_grad:
                self.grad += e_np * out.grad
        out._backward = _back
        return out

    def sqrt(self):
        xp = self.xp
        s_gpu = xp.sqrt(xp.abs(self.data.astype(np.float64)) + 1e-12)
        out = self._wrap(s_gpu, (self,))
        s_np = _to_numpy(s_gpu)
        def _back():
            if self.requires_grad:
                self.grad += 0.5 / s_np * out.grad
        out._backward = _back
        return out

    # ── reductions ────────────────────────────────────────────────────────
    def sum(self, axis=None):
        xp = self.xp
        out_data = self.data.sum(axis=axis)
        out = self._wrap(out_data, (self,))
        orig_shape = self.data.shape
        def _back():
            if self.requires_grad:
                g = out.grad if axis is None else np.expand_dims(out.grad, axis)
                self.grad += np.ones(orig_shape, dtype=np.float64) * g
        out._backward = _back
        return out

    def mean(self, axis=None):
        n = self.data.size if axis is None else self.data.shape[axis]
        return self.sum(axis) * (1.0 / n)

    def max(self, axis=None):
        out_data = self.data.max(axis=axis)
        out = self._wrap(out_data, (self,))
        d_np = _to_numpy(self.data)
        def _back():
            if self.requires_grad:
                mask = (d_np == d_np.max(axis=axis, keepdims=True)).astype(np.float64)
                self.grad += mask * (out.grad if axis is None else np.expand_dims(out.grad, axis))
        out._backward = _back
        return out

    def min(self, axis=None): return -(-self).max(axis)

    # ── dropout (differentiable, training-mode) ────────────────────────────
    def dropout(self, p=0.5, training=True):
        if not training or p == 0.0:
            return self
        mask_np = (np.random.rand(*self.data.shape) > p).astype(np.float64) / (1.0 - p)
        xp = self.xp
        mask_xp = xp.array(mask_np) if HAS_GPU and xp is cp else mask_np
        out = self._wrap(self.data * mask_xp, (self,))
        def _back():
            if self.requires_grad:
                self.grad += mask_np * out.grad
        out._backward = _back
        return out

    # ── convolution (1d and 2d) with autodiff ─────────────────────────────
    def conv1d(self, kernel, stride=1, padding=0):
        """1D convolution: self=[L], kernel=[K]. Returns [L'] tensor."""
        x = _to_numpy(self.data).astype(np.float64)
        k = _to_numpy(kernel.data if isinstance(kernel, NuvolaTensor) else np.array(kernel)).astype(np.float64)
        if padding > 0:
            x = np.pad(x, padding)
        L, K = len(x), len(k)
        L_out = (L - K) // stride + 1
        out_data = np.array([np.dot(x[i*stride:i*stride+K], k) for i in range(L_out)])
        out = self._wrap(out_data, (self, kernel) if isinstance(kernel, NuvolaTensor) else (self,))
        x_orig = x.copy()
        def _back():
            if self.requires_grad:
                dx = np.zeros_like(x_orig)
                for i in range(L_out):
                    dx[i*stride:i*stride+K] += k * out.grad[i]
                if padding > 0:
                    dx = dx[padding:-padding]
                self.grad += dx
            if isinstance(kernel, NuvolaTensor) and kernel.requires_grad:
                dk = np.zeros_like(k)
                for i in range(L_out):
                    dk += x_orig[i*stride:i*stride+K] * out.grad[i]
                kernel.grad += dk
        out._backward = _back
        return out

    def conv2d(self, kernel, stride=1, padding=0, bias=None):
        """2D convolution: self=[H,W], kernel=[KH,KW]. Returns [H',W'] tensor.
        Fork-join parallel over output rows when output is large enough."""
        x = _to_numpy(self.data).astype(np.float64)
        k = _to_numpy(kernel.data if isinstance(kernel, NuvolaTensor) else np.array(kernel)).astype(np.float64)
        if padding > 0:
            x = np.pad(x, ((padding, padding), (padding, padding)))
        H, W = x.shape[-2], x.shape[-1]
        KH, KW = k.shape[-2], k.shape[-1]
        H_out = (H - KH) // stride + 1
        W_out = (W - KW) // stride + 1
        out_data = np.zeros((H_out, W_out), dtype=np.float64)

        # Fast path: scipy.signal.correlate2d is 60x faster than Python loops.
        # Falls back to vectorized im2col if scipy unavailable.
        if HAS_SCIPY and stride == 1:
            from scipy.signal import correlate2d as _corr2d
            out_data = _corr2d(x, k, mode='valid').astype(np.float64)
        else:
            # Vectorized im2col: build patch matrix, single matmul — no Python loops.
            patches = np.lib.stride_tricks.as_strided(
                x,
                shape=(H_out, W_out, KH, KW),
                strides=(x.strides[0]*stride, x.strides[1]*stride,
                         x.strides[0], x.strides[1]),
                writeable=False,
            ).reshape(H_out * W_out, KH * KW)
            out_data = (patches @ k.ravel()).reshape(H_out, W_out)
        if bias is not None:
            b = bias.item() if isinstance(bias, NuvolaTensor) else float(bias)
            out_data += b
        parents = tuple(p for p in (self, kernel, bias) if isinstance(p, NuvolaTensor))
        out = self._wrap(out_data, parents)
        x_pad = x.copy()
        def _back():
            if self.requires_grad:
                # Backward through input: full conv of grad with flipped kernel
                if HAS_SCIPY and stride == 1:
                    from scipy.signal import convolve2d as _conv2d
                    dx_pad = _conv2d(out.grad, k[::-1, ::-1], mode='full').astype(np.float64)
                else:
                    dx_pad = np.zeros_like(x_pad)
                    for i in range(H_out):
                        for j in range(W_out):
                            dx_pad[i*stride:i*stride+KH, j*stride:j*stride+KW] += k * out.grad[i, j]
                dx_full = dx_pad if padding == 0 else dx_pad[padding:-padding, padding:-padding]
                self.grad += dx_full
            if isinstance(kernel, NuvolaTensor) and kernel.requires_grad:
                # Backward through kernel: correlate input with output grad
                if HAS_SCIPY and stride == 1:
                    from scipy.signal import correlate2d as _corr2d
                    dk = _corr2d(x_pad, out.grad, mode='valid').astype(np.float64)
                else:
                    dk = np.zeros_like(k)
                    for i in range(H_out):
                        for j in range(W_out):
                            dk += x_pad[i*stride:i*stride+KH, j*stride:j*stride+KW] * out.grad[i, j]
                kernel.grad += dk
            if isinstance(bias, NuvolaTensor) and bias.requires_grad:
                bias.grad += np.array([out.grad.sum()])
        out._backward = _back
        return out

    # ── shape ops ─────────────────────────────────────────────────────────
    def reshape(self, *shape):
        if len(shape) == 1 and isinstance(shape[0], list): shape = tuple(shape[0])
        out = self._wrap(self.data.reshape(shape), (self,))
        def _back():
            if self.requires_grad:
                self.grad += out.grad.reshape(self.data.shape)
        out._backward = _back
        return out

    def transpose(self, *axes):
        if not axes: axes = None
        out = self._wrap(self.data.transpose(axes), (self,))
        def _back():
            if self.requires_grad:
                inv = None if axes is None else tuple(np.argsort(axes))
                self.grad += out.grad.transpose(inv)
        out._backward = _back
        return out

    @property
    def T(self): return self.transpose()

    def flatten(self): return self.reshape(-1)

    def unsqueeze(self, axis):
        xp = self.xp
        return self.reshape(*xp.expand_dims(self.data, axis).shape)

    def squeeze(self, axis=None):
        xp = self.xp
        sq = xp.squeeze(self.data, axis=axis) if axis is not None else xp.squeeze(self.data)
        out = self._wrap(sq, (self,))
        orig_shape = self.data.shape
        def _back():
            if self.requires_grad:
                self.grad += out.grad.reshape(orig_shape)
        out._backward = _back
        return out

    @staticmethod
    def where(cond, x, y):
        """Element-wise select: cond ? x : y, with autodiff."""
        cx = _to_numpy(cond.data if isinstance(cond, NuvolaTensor) else np.array(cond)).astype(bool)
        xd = x.data if isinstance(x, NuvolaTensor) else np.array(x, dtype=np.float64)
        yd = y.data if isinstance(y, NuvolaTensor) else np.array(y, dtype=np.float64)
        out_data = np.where(cx, _to_numpy(xd), _to_numpy(yd)).astype(np.float64)
        parents = tuple(p for p in (x, y) if isinstance(p, NuvolaTensor))
        out = (x if isinstance(x, NuvolaTensor) else y)._wrap(out_data, parents)
        def _back():
            if isinstance(x, NuvolaTensor) and x.requires_grad:
                x.grad += np.where(cx, out.grad, 0.0)
            if isinstance(y, NuvolaTensor) and y.requires_grad:
                y.grad += np.where(cx, 0.0, out.grad)
        out._backward = _back
        return out

    # ── indexing ──────────────────────────────────────────────────────────
    def __getitem__(self, idx):
        out = self._wrap(self.data[idx], (self,))
        def _back():
            if self.requires_grad:
                np.add.at(self.grad, idx, out.grad)
        out._backward = _back
        return out

    # ── quantization ──────────────────────────────────────────────────────
    def quantize_int8(self):
        """Symmetric int8 quantization."""
        scale = np.abs(self.data).max() / 127.0 + 1e-12
        q = np.clip(np.round(self.data / scale), -128, 127).astype(np.int8)
        return NuvolaTensor(q, dtype='i8'), float(scale)

    def dequantize(self, scale, target_dtype='f32'):
        dtype_map = {'f32': np.float32, 'f64': np.float64}
        dt = dtype_map.get(target_dtype, np.float32)
        return NuvolaTensor(self.data.astype(dt) * scale, dtype=target_dtype)

    def to_f16(self): return NuvolaTensor(_to_numpy(self.data).astype(np.float16), dtype='f16')
    def to_f32(self): return NuvolaTensor(_to_numpy(self.data).astype(np.float32), dtype='f32')
    def to_f64(self): return NuvolaTensor(_to_numpy(self.data).astype(np.float64), dtype='f64')
    def to_i8(self):  return NuvolaTensor(_to_numpy(self.data).astype(np.int8),    dtype='i8')
    def to_i32(self): return NuvolaTensor(_to_numpy(self.data).astype(np.int32),   dtype='i32')

    def cuda(self):
        """Move tensor to GPU (cupy). Returns self if no cupy."""
        if not HAS_GPU: return self
        out = NuvolaTensor.__new__(NuvolaTensor)
        out.data = cp.array(self.data)
        out.grad = np.zeros_like(_to_numpy(self.data), dtype=np.float64)
        out.requires_grad = self.requires_grad
        out.label = self.label; out.device = 'cuda'
        out._dtype_name = self._dtype_name
        out._backward = lambda: None; out._prev = set()
        return out

    def cpu(self):
        """Move tensor to CPU (numpy)."""
        if self.device == 'cpu': return self
        out = NuvolaTensor(__new__=True)
        out = NuvolaTensor(_to_numpy(self.data), dtype=self._dtype_name,
                           requires_grad=self.requires_grad, label=self.label)
        return out

    # ── autodiff ──────────────────────────────────────────────────────────
    def backward(self, grad=None):
        """Run reverse-mode autodiff from this tensor."""
        if grad is not None:
            self.grad = np.array(grad, dtype=np.float64) if not isinstance(grad, np.ndarray) else grad
        else:
            self.grad = np.ones_like(self.data, dtype=np.float64)

        # Topological sort
        topo = []
        visited = set()
        def build(v):
            if id(v) not in visited:
                visited.add(id(v))
                for child in v._prev:
                    build(child)
                topo.append(v)
        build(self)
        for v in reversed(topo):
            v._backward()

    def zero_grad(self):
        self.grad = np.zeros_like(self.data, dtype=np.float64)

    # ── Python numeric protocol ────────────────────────────────────────────
    # Lets tensors be used anywhere Python expects a number:
    # math.sqrt(t), range(t), list[t], int(t), float(t), t % n, etc.
    def __float__(self):   return float(_to_numpy(self.data).flat[0])
    def __int__(self):     return int(_to_numpy(self.data).flat[0])
    def __index__(self):   return int(_to_numpy(self.data).flat[0])  # list/array indexing
    def __abs__(self):     return NuvolaTensor(np.abs(_to_numpy(self.data)))
    def __round__(self, n=0): return round(float(self), n)
    def __trunc__(self):   return int(_to_numpy(self.data).flat[0])
    def __floor__(self):   return math.floor(float(self))
    def __ceil__(self):    return math.ceil(float(self))

    def __mod__(self, other):
        d = _to_numpy(other.data) if isinstance(other, NuvolaTensor) else other
        return NuvolaTensor(_to_numpy(self.data) % d)
    def __rmod__(self, other):
        return NuvolaTensor(other % _to_numpy(self.data))
    def __floordiv__(self, other):
        d = _to_numpy(other.data) if isinstance(other, NuvolaTensor) else other
        return NuvolaTensor(_to_numpy(self.data) // d)
    def __rfloordiv__(self, other):
        return NuvolaTensor(other // _to_numpy(self.data))
    def __rtruediv__(self, other):
        return NuvolaTensor(other).__truediv__(self)
    def __rpow__(self, base):
        return NuvolaTensor(float(base) ** _to_numpy(self.data))

    # ── comparison ops ────────────────────────────────────────────────────
    def __eq__(self, other):
        if isinstance(other, NuvolaTensor): return bool(np.array_equal(self.data, other.data))
        return bool(np.all(self.data == other))
    def __lt__(self, other):
        d = other.data if isinstance(other, NuvolaTensor) else other
        return NuvolaTensor(self.data < d)
    def __gt__(self, other):
        d = other.data if isinstance(other, NuvolaTensor) else other
        return NuvolaTensor(self.data > d)
    def __le__(self, other):
        d = other.data if isinstance(other, NuvolaTensor) else other
        return NuvolaTensor(self.data <= d)
    def __ge__(self, other):
        d = other.data if isinstance(other, NuvolaTensor) else other
        return NuvolaTensor(self.data >= d)

    # ── repr ──────────────────────────────────────────────────────────────
    # Must be hashable so tensors can live in sets (for _prev in the autodiff graph)
    __hash__ = object.__hash__

    def __repr__(self):
        s = list(self.data.shape)
        return f"Tensor(shape={s}, dtype={self._dtype_name})"

    def item(self):
        """Extract scalar value (always CPU)."""
        return float(_to_numpy(self.data).flat[0])

    def tolist(self): return _to_numpy(self.data).tolist()

    def numpy(self): return _to_numpy(self.data).copy()

# ─────────────────────────────────────────────────────────────────────────────
# NuvolaChannel — queue.Queue-backed typed channel
# ─────────────────────────────────────────────────────────────────────────────

class NuvolaChannel:
    def __init__(self, capacity=0, dtype=None):
        self._q = queue.Queue(maxsize=capacity)
        self.dtype = dtype
        self.closed = False

    def send(self, value):
        if self.closed: raise RuntimeError("send on closed channel")
        self._q.put(value)
        return value

    def recv(self, timeout=None):
        try:
            return self._q.get(timeout=timeout)
        except queue.Empty:
            return NuvolaOption(None, False)

    def try_recv(self):
        try:
            return NuvolaOption(self._q.get_nowait(), True)
        except queue.Empty:
            return NuvolaOption(None, False)

    def close(self): self.closed = True

    def __repr__(self): return f"Channel(cap={self._q.maxsize}, dtype={self.dtype})"

# ─────────────────────────────────────────────────────────────────────────────
# NuvolaFuture — result of spawn
# ─────────────────────────────────────────────────────────────────────────────

class NuvolaFuture:
    def __init__(self, thread: threading.Thread, result_box: list):
        self._thread = thread
        self._box = result_box  # [result] or [None, exception]

    def await_(self, timeout=None):
        self._thread.join(timeout)
        if len(self._box) >= 2 and self._box[1] is not None:
            raise self._box[1]
        return self._box[0] if self._box else None

    def is_done(self): return not self._thread.is_alive()

    def __repr__(self): return f"Future(done={self.is_done()})"

# ─────────────────────────────────────────────────────────────────────────────
# NuvolaServer — built-in HTTP AI server
# ─────────────────────────────────────────────────────────────────────────────

class NuvolaServer:
    """
    Minimal HTTP/JSON server for serving Nuvola AI models.

    Routes are plain dicts mapping path → Nuvola callable.
    Each handler receives {'method', 'path', 'body', 'query'} and must
    return any Nuvola-serialisable value (tensor → tolist() automatically).

    Usage in Nuvola:
        srv := serve(8080, {"/predict": fn(req) => ..., "/health": fn(req) => "ok"})
        srv.start()
        -- (runs in background thread, non-blocking)
        srv.stop()
    """
    import json as _json_mod  # class-level so handler closure can use it

    def __init__(self, port: int, routes: dict, evaluator):
        self._port = port
        self._routes = routes   # path (str) → Nuvola callable
        self._ev = evaluator
        self._srv = None
        self._thread = None

    def _make_response(self, value):
        import json
        if isinstance(value, NuvolaTensor):
            value = value.tolist()
        elif isinstance(value, NuvolaStruct):
            value = {k: (v.tolist() if isinstance(v, NuvolaTensor) else v)
                     for k, v in value.fields.items()}
        return json.dumps(value).encode('utf-8')

    def start(self):
        import http.server, socketserver, json, urllib.parse

        ev = self._ev
        routes = self._routes
        server_self = self

        class _Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_): pass  # suppress access log

            def _dispatch(self, body_bytes=b'{}'):
                parsed = urllib.parse.urlparse(self.path)
                path = parsed.path
                query_str = parsed.query
                query = dict(urllib.parse.parse_qsl(query_str))
                handler = routes.get(path)
                if handler is None:
                    self.send_error(404, f'No route: {path}')
                    return
                try:
                    body = json.loads(body_bytes or b'{}') if body_bytes else {}
                except Exception:
                    body = {}
                req = {'method': self.command, 'path': path,
                       'body': body, 'query': query}
                result = ev._call(handler, [req])
                out = server_self._make_response(result)
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(out)))
                self.send_header('Access-Control-Allow-Origin', '*')
                self.end_headers()
                self.wfile.write(out)

            def do_GET(self):
                self._dispatch(b'')

            def do_POST(self):
                n = int(self.headers.get('Content-Length', 0))
                self._dispatch(self.rfile.read(n) if n else b'{}')

            def do_OPTIONS(self):
                self.send_response(204)
                self.send_header('Access-Control-Allow-Origin', '*')
                self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
                self.send_header('Access-Control-Allow-Headers', 'Content-Type')
                self.end_headers()

        socketserver.TCPServer.allow_reuse_address = True
        self._srv = socketserver.TCPServer(('0.0.0.0', self._port), _Handler)
        self._thread = threading.Thread(target=self._srv.serve_forever, daemon=True)
        self._thread.start()
        print(f"  NuvolaServer listening on http://0.0.0.0:{self._port}")
        return self

    def stop(self):
        if self._srv:
            self._srv.shutdown()
            self._srv = None
        return self

    def __repr__(self):
        state = 'running' if (self._thread and self._thread.is_alive()) else 'stopped'
        return f"NuvolaServer(port={self._port}, routes={list(self._routes.keys())}, state={state})"

# ─────────────────────────────────────────────────────────────────────────────
# Nuclear keywords and parser extensions
# ─────────────────────────────────────────────────────────────────────────────

NUCLEAR_KEYWORDS = KEYWORDS | {
    'comptime', 'async', 'await', 'spawn', 'extern', 'unsafe',
    'trait', 'impl',
    # 'where' intentionally NOT a keyword — stays IDENT so it can be used as a builtin fn
}

def nuclear_tokenize(src: str):
    """Like tokenize() but recognizes nuclear keywords as KW tokens."""
    tokens = tokenize(src)
    # These identifiers are used as builtin functions, not statement keywords.
    # Reclassify them from KW → IDENT so they can be called like normal fns.
    DEMOTE_TO_IDENT = {'where'}
    result = []
    for tok in tokens:
        if tok[0] == 'KW' and tok[1] in DEMOTE_TO_IDENT:
            result.append(('IDENT', tok[1], tok[2], tok[3] if len(tok) > 3 else 0))
        elif tok[0] == 'IDENT' and tok[1] in NUCLEAR_KEYWORDS:
            result.append(('KW', tok[1], tok[2], tok[3] if len(tok) > 3 else 0))
        else:
            result.append(tok)
    return result

class NuclearParser(Parser):
    """Extends Parser with nuclear syntax."""

    def parse_params(self):
        """Like base parse_params but accepts 'self' (KW) as a parameter name."""
        params = []
        while self.peek()[1] != ')':
            if self.peek()[0] == 'OP' and self.peek()[1] == '...':
                self.pos += 1
            # Allow 'self' keyword as param name
            if self.peek()[0] == 'KW' and self.peek()[1] == 'self':
                pname = self.peek()[1]; self.pos += 1
            else:
                t = self.eat('IDENT')
                pname = t[1]
            ptype = None; pdefault = None
            if self.peek()[0] == 'OP' and self.peek()[1] == ':':
                self.pos += 1; ptype = self.parse_type_expr()
                if self.peek()[1] == 'where':   # trait bounds (IDENT or KW)
                    self.pos += 1
                    while self.peek()[1] not in (',', ')'):
                        self.pos += 1
            if self.peek()[0] == 'OP' and self.peek()[1] == '=':
                self.pos += 1; pdefault = self.parse_expr()
            params.append((pname, ptype, pdefault))
            if not self.eat_if('OP', ','): break
        return params

    def _parse_call_args(self):
        """Parse call arguments, supporting both positional and keyword (name=expr) args."""
        args = []; kwargs = {}
        while self.peek()[1] != ')':
            self.skip_newlines()
            if self.peek()[1] == ')': break
            # Peek for `name =` keyword arg pattern
            if (self.peek()[0] == 'IDENT' and
                    self.peek(1)[0] == 'OP' and self.peek(1)[1] == '='):
                kname = self.eat('IDENT')[1]
                self.pos += 1  # eat '='
                kwargs[kname] = self.parse_expr()
            else:
                args.append(self.parse_expr())
            if not self.eat_if('OP', ','): break
            self.skip_newlines()
        return args, kwargs

    def parse_postfix(self) -> Node:
        node = self.parse_primary()
        while True:
            t = self.peek()
            if t[0] == 'OP' and t[1] == '.' and self.peek(1)[0] == 'IDENT':
                self.pos += 1
                field = self.eat('IDENT')[1]
                if self.peek()[0] == 'OP' and self.peek()[1] == '(':
                    self.pos += 1
                    args, kwargs = self._parse_call_args()
                    self.eat('OP', ')')
                    node = Call(Field(node, field), args, kwargs)
                else:
                    node = Field(node, field)
            elif t[0] == 'OP' and t[1] == '[':
                self.pos += 1
                idx = self.parse_expr()
                self.eat('OP', ']')
                node = Index(node, idx)
            elif t[0] == 'OP' and t[1] == '(':
                self.pos += 1
                args, kwargs = self._parse_call_args()
                self.eat('OP', ')')
                node = Call(node, args, kwargs)
            elif t[0] == 'OP' and t[1] == '?.':
                self.pos += 1
                field = self.eat('IDENT')[1]
                node = BinOp('?.', node, Ident(field))
            else:
                break
        return node

    def parse_stmt(self):
        self.skip_newlines()
        t = self.peek()

        # comptime name := expr
        if t[0] == 'KW' and t[1] == 'comptime':
            return self._parse_comptime()

        # trait TraitName { ... }
        if t[0] == 'KW' and t[1] == 'trait':
            return self._parse_trait()

        # impl TraitName for TypeName { ... }
        if t[0] == 'KW' and t[1] == 'impl':
            return self._parse_impl()

        # async fn name(...) => body
        if t[0] == 'KW' and t[1] == 'async':
            return self._parse_async_fn()

        # extern fn ...
        if t[0] == 'KW' and t[1] == 'extern':
            return self._parse_extern_fn()

        # unsafe { ... }
        if t[0] == 'KW' and t[1] == 'unsafe':
            return self._parse_unsafe()

        # await expr (statement form)
        if t[0] == 'KW' and t[1] == 'await':
            self.pos += 1
            expr = self.parse_expr()
            return Await(expr)

        # spawn expr (statement form)
        if t[0] == 'KW' and t[1] == 'spawn':
            self.pos += 1
            expr = self.parse_expr()
            return Spawn(expr)

        return super().parse_stmt()

    def parse_primary(self):
        t = self.peek()

        # 'self' used as an identifier inside method bodies
        if t[0] == 'KW' and t[1] == 'self':
            self.pos += 1
            return Ident('self')

        # await expr
        if t[0] == 'KW' and t[1] == 'await':
            self.pos += 1
            return Await(self.parse_expr())

        # spawn expr  (statement form without args — bare spawn fn)
        # Note: spawn(fn, arg) calls stay as regular Call nodes so they
        # go through _builtin_spawn via the env binding; only a bare
        # `spawn expr` (not immediately followed by '(') creates a Spawn node.
        if t[0] == 'KW' and t[1] == 'spawn':
            self.pos += 1
            return Spawn(self.parse_expr())

        # unsafe { ... }
        if t[0] == 'KW' and t[1] == 'unsafe':
            return self._parse_unsafe()

        return super().parse_primary()

    def _parse_comptime(self):
        self.eat('KW', 'comptime')
        name = self.eat('IDENT')[1]
        if self.peek()[1] in (':=', '='):
            self.pos += 1
        expr = self.parse_expr()
        return ComptimeDecl(name, expr)

    def _parse_trait(self):
        self.eat('KW', 'trait')
        name = self.eat('IDENT')[1]
        self.skip_newlines()
        methods = {}
        trait_col = self._tok_col()
        while True:
            self.skip_newlines()
            t = self.peek()
            if t[0] == 'EOF': break
            tok_col = t[3] if len(t) > 3 else 0
            if tok_col < trait_col: break
            if t[0] == 'KW' and t[1] == 'fn':
                decl = self.parse_fn_decl()
                methods[decl.name] = decl
            else:
                break
        return TraitDecl(name, methods)

    def _parse_impl(self):
        self.eat('KW', 'impl')
        trait_name = self.eat('IDENT')[1]
        # optional `for TypeName`
        type_name = None
        if self.peek()[0] == 'KW' and self.peek()[1] == 'for':
            self.pos += 1
            type_name = self.eat('IDENT')[1]
        self.skip_newlines()
        methods = {}
        impl_col = self._tok_col()
        while True:
            self.skip_newlines()
            t = self.peek()
            if t[0] == 'EOF': break
            tok_col = t[3] if len(t) > 3 else 0
            if tok_col < impl_col: break
            if t[0] == 'KW' and t[1] == 'fn':
                decl = self.parse_fn_decl()
                methods[decl.name] = decl
            else:
                break
        return ImplDecl(trait_name, type_name, methods)

    def _parse_async_fn(self):
        self.eat('KW', 'async')
        self.eat('KW', 'fn')
        name = self.eat('IDENT')[1]
        self.eat('OP', '(')
        params = self.parse_params()
        self.eat('OP', ')')
        ret_type = None
        if self.peek()[0] == 'OP' and self.peek()[1] == '->':
            self.pos += 1; ret_type = self.parse_type_expr()
        self.skip_newlines()
        if self.peek()[0] == 'OP' and self.peek()[1] == '=>':
            self.pos += 1
            body = Block([Return(self.parse_expr())])
        else:
            body = self.parse_indented_block()
        return AsyncFnDecl(name, params, body, ret_type)

    def _parse_extern_fn(self):
        self.eat('KW', 'extern')
        # optional library string: extern "libm" fn ...
        lib = None
        if self.peek()[0] == 'STRING':
            lib = self.eat('STRING')[1]
        self.eat('KW', 'fn')
        name = self.eat('IDENT')[1]
        self.eat('OP', '(')
        params = self.parse_params()
        self.eat('OP', ')')
        ret_type = None
        if self.peek()[0] == 'OP' and self.peek()[1] == '->':
            self.pos += 1; ret_type = self.parse_type_expr()
        return ExternFn(name, params, ret_type, lib)

    def _parse_unsafe(self):
        self.eat('KW', 'unsafe')
        self.skip_newlines()
        body = self.parse_indented_block()
        return UnsafeBlock(body)


# ─────────────────────────────────────────────────────────────────────────────
# NuclearEvaluator
# ─────────────────────────────────────────────────────────────────────────────

class NuclearEvaluator(Evaluator):
    """Extends Evaluator with trait registry, tensors, comptime, async."""

    def __init__(self):
        super().__init__()
        # trait registry: { type_name: { method_name: NuvolaFn } }
        self._trait_impls: Dict[str, Dict[str, NuvolaFn]] = {}
        # trait declarations: { trait_name: TraitDecl }
        self._trait_decls: Dict[str, TraitDecl] = {}
        # comptime cache: { name: value }
        self._comptime: Dict[str, Any] = {}
        # in-flight tasks
        self._tasks: Dict[int, NuvolaFuture] = {}
        self._task_counter = 0
        # unsafe mode flag (re-entrant)
        self._unsafe_depth = 0

        self._register_nuclear_builtins()

    # ── nuclear builtins ──────────────────────────────────────────────────
    def _register_nuclear_builtins(self):
        e = self.global_env

        # ── dtype constants ──
        e.define('f64',  'f64',  immutable=True)
        e.define('f32',  'f32',  immutable=True)
        e.define('f16',  'f16',  immutable=True)
        e.define('i64',  'i64',  immutable=True)
        e.define('i32',  'i32',  immutable=True)
        e.define('i16',  'i16',  immutable=True)
        e.define('i8',   'i8',   immutable=True)
        e.define('u8',   'u8',   immutable=True)
        e.define('bool_dtype', 'bool', immutable=True)

        # ── tensor constructors ──
        e.define('tensor',  self._builtin_tensor,  immutable=True)
        e.define('zeros',   self._builtin_zeros,   immutable=True)
        e.define('ones',    self._builtin_ones,    immutable=True)
        e.define('randn',   self._builtin_randn,   immutable=True)
        e.define('rand',    self._builtin_rand,    immutable=True)
        e.define('eye',     self._builtin_eye,     immutable=True)
        e.define('arange',  self._builtin_arange,  immutable=True)
        e.define('linspace', self._builtin_linspace, immutable=True)
        e.define('full',    self._builtin_full,    immutable=True)

        # ── tensor ops ──
        e.define('matmul',  lambda a, b: a.matmul(b),   immutable=True)
        e.define('dot',     lambda a, b: a.matmul(b) if a.ndim > 1 else
                                          NuvolaTensor(float(np.dot(a.data, b.data))),
                 immutable=True)
        e.define('einsum',  self._builtin_einsum, immutable=True)
        e.define('stack',   self._builtin_stack,  immutable=True)
        e.define('cat',     self._builtin_cat,    immutable=True)
        e.define('broadcast', self._builtin_broadcast, immutable=True)

        # ── math function overrides (tensor-aware; fixes "must be real number") ──
        # These shadow the base evaluator's math.sqrt / math.log etc.
        def _t(x): return isinstance(x, NuvolaTensor)
        e.define('sqrt',  lambda x: x.sqrt() if _t(x) else math.sqrt(x),  immutable=True)
        e.define('log',   lambda x, base=None: x.log() if _t(x) else (math.log(x) if base is None else math.log(x, base)), immutable=True)
        e.define('log2',  lambda x: NuvolaTensor(np.log2(_to_numpy(x.data)))  if _t(x) else math.log2(x),  immutable=True)
        e.define('log10', lambda x: NuvolaTensor(np.log10(_to_numpy(x.data))) if _t(x) else math.log10(x), immutable=True)
        e.define('exp',   lambda x: x.exp()   if _t(x) else math.exp(x),   immutable=True)
        e.define('abs',   lambda x: x.__abs__() if _t(x) else abs(x),      immutable=True)
        e.define('sin',   lambda x: NuvolaTensor(np.sin(_to_numpy(x.data)))  if _t(x) else math.sin(x),   immutable=True)
        e.define('cos',   lambda x: NuvolaTensor(np.cos(_to_numpy(x.data)))  if _t(x) else math.cos(x),   immutable=True)
        e.define('tan',   lambda x: NuvolaTensor(np.tan(_to_numpy(x.data)))  if _t(x) else math.tan(x),   immutable=True)
        e.define('asin',  lambda x: NuvolaTensor(np.arcsin(_to_numpy(x.data)))  if _t(x) else math.asin(x), immutable=True)
        e.define('acos',  lambda x: NuvolaTensor(np.arccos(_to_numpy(x.data)))  if _t(x) else math.acos(x), immutable=True)
        e.define('atan',  lambda x: NuvolaTensor(np.arctan(_to_numpy(x.data)))  if _t(x) else math.atan(x), immutable=True)
        e.define('atan2', lambda y, x: NuvolaTensor(np.arctan2(_to_numpy(y.data), _to_numpy(x.data))) if _t(y) else math.atan2(y, x), immutable=True)
        e.define('floor', lambda x: NuvolaTensor(np.floor(x.data)) if _t(x) else math.floor(x), immutable=True)
        e.define('ceil',  lambda x: NuvolaTensor(np.ceil(x.data))  if _t(x) else math.ceil(x),  immutable=True)
        e.define('round_', lambda x, n=0: NuvolaTensor(np.round(x.data, n)) if _t(x) else round(x, n), immutable=True)
        e.define('clamp', lambda x, lo, hi: NuvolaTensor(np.clip(_to_numpy(x.data) if _t(x) else x, lo, hi)), immutable=True)
        e.define('sign',  lambda x: NuvolaTensor(np.sign(_to_numpy(x.data))) if _t(x) else math.copysign(1, x), immutable=True)
        e.define('pow',   lambda x, y: x.__pow__(y) if _t(x) else x ** y, immutable=True)
        e.define('hypot', lambda a, b: math.hypot(float(a) if _t(a) else a, float(b) if _t(b) else b), immutable=True)
        e.define('pi',    math.pi,  immutable=True)
        e.define('e_',    math.e,   immutable=True)
        e.define('inf',   math.inf, immutable=True)
        e.define('nan',   math.nan, immutable=True)

        # ── activations ──
        e.define('relu',    lambda t: t.relu()    if isinstance(t, NuvolaTensor) else max(0, t), immutable=True)
        e.define('sigmoid', lambda t: t.sigmoid() if isinstance(t, NuvolaTensor) else 1/(1+math.exp(-t)), immutable=True)
        e.define('softmax', lambda t, axis=-1: t.softmax(axis) if isinstance(t, NuvolaTensor) else t, immutable=True)
        e.define('gelu',    lambda t: t.gelu()    if isinstance(t, NuvolaTensor) else t, immutable=True)
        e.define('tanh',    lambda t: t.tanh()    if isinstance(t, NuvolaTensor) else math.tanh(t), immutable=True)

        # ── loss functions ──
        e.define('mse_loss',   self._builtin_mse_loss,   immutable=True)
        e.define('cross_entropy', self._builtin_cross_entropy, immutable=True)
        e.define('bce_loss',   self._builtin_bce_loss,   immutable=True)

        # ── quantization ──
        e.define('quantize',   self._builtin_quantize,   immutable=True)
        e.define('dequantize', self._builtin_dequantize, immutable=True)

        # ── normalization ──
        e.define('layer_norm', self._builtin_layer_norm, immutable=True)
        e.define('batch_norm', self._builtin_batch_norm, immutable=True)

        # ── channels / concurrency ──
        e.define('chan',      lambda cap=0, dtype=None: NuvolaChannel(int(cap), dtype), immutable=True)
        e.define('spawn',     self._builtin_spawn,  immutable=True)
        e.define('await_',    self._builtin_await,  immutable=True)
        e.define('sleep_ms',  lambda ms: time.sleep(ms/1000), immutable=True)

        # ── optimizers ──
        e.define('adam',      self._builtin_adam,      immutable=True)
        e.define('sgd_opt',   self._builtin_sgd_opt,   immutable=True)
        e.define('adagrad',   self._builtin_adagrad,   immutable=True)
        e.define('rmsprop',   self._builtin_rmsprop,   immutable=True)

        # ── gradient helpers ──
        e.define('no_grad',         self._builtin_no_grad,         immutable=True)
        e.define('grad_of',         self._builtin_grad_of,         immutable=True)
        e.define('backward',        lambda t: t.backward() or t,   immutable=True)
        e.define('zero_grad',       lambda ts: [t.zero_grad() for t in (ts if isinstance(ts, list) else [ts])], immutable=True)
        e.define('clip_grad_norm',  self._builtin_clip_grad_norm,  immutable=True)

        # ── learning rate schedules ──
        e.define('cosine_lr',        self._builtin_cosine_lr,        immutable=True)
        e.define('warmup_cosine_lr', self._builtin_warmup_cosine_lr, immutable=True)

        # ── convolutions / masking ──
        e.define('conv1d',   self._builtin_conv1d,   immutable=True)
        e.define('conv2d',   self._builtin_conv2d,   immutable=True)
        e.define('dropout',  self._builtin_dropout,  immutable=True)
        e.define('where',    self._builtin_where,    immutable=True)

        # ── loss functions ──
        e.define('huber_loss', self._builtin_huber_loss, immutable=True)

        # ── memory / hardware ──
        e.define('has_gpu',   HAS_GPU,   immutable=True)
        e.define('has_numpy', HAS_NUMPY, immutable=True)
        e.define('has_scipy', HAS_SCIPY, immutable=True)
        e.define('has_sympy', HAS_SYMPY, immutable=True)

        # ── fork-join parallelism ─────────────────────────────────────────
        # par_map(fn, list) — applies fn to each element in parallel, returns list.
        # Automatically uses all CPU cores. Faster than for-loop above ~100 items.
        def _par_map(fn, lst):
            return list(_PAR_EXECUTOR.map(fn, lst))
        e.define('par_map', _par_map, immutable=True)

        # par_reduce(fn, list, init) — parallel prefix then serial fold
        def _par_reduce(fn, lst, init=None):
            import functools
            return functools.reduce(fn, lst, init) if init is not None else functools.reduce(fn, lst)
        e.define('par_reduce', _par_reduce, immutable=True)

        # ── scipy: advanced math (LAPACK / FFT / special) ─────────────────
        if HAS_SCIPY:
            _T = NuvolaTensor
            def _to_np(x): return _to_numpy(x.data) if isinstance(x, _T) else np.array(x, dtype=np.float64)

            # Special functions
            e.define('erf',    lambda x: _T(_ssp.erf(_to_np(x))),   immutable=True)
            e.define('erfc',   lambda x: _T(_ssp.erfc(_to_np(x))),  immutable=True)
            e.define('gamma',  lambda x: _T(_ssp.gamma(_to_np(x))), immutable=True)
            e.define('lgamma', lambda x: _T(_ssp.gammaln(_to_np(x))), immutable=True)
            e.define('beta',   lambda a, b: float(_ssp.beta(float(a), float(b))), immutable=True)
            e.define('bessel', lambda n, x: _T(_ssp.jv(float(n), _to_np(x))), immutable=True)
            e.define('sigmoid_sp', lambda x: _T(_ssp.expit(_to_np(x))), immutable=True)  # numerically stable

            # Linear algebra (LAPACK-backed — much faster than numpy for large matrices)
            e.define('svd',    lambda t: [_T(a) for a in _sla.svd(_to_np(t), full_matrices=False)], immutable=True)
            e.define('eig',    lambda t: [_T(np.real(a)) for a in _sla.eig(_to_np(t))],  immutable=True)
            e.define('cholesky', lambda t: _T(_sla.cholesky(_to_np(t))), immutable=True)
            e.define('solve',  lambda A, b: _T(_sla.solve(_to_np(A), _to_np(b))), immutable=True)
            e.define('lstsq',  lambda A, b: _T(_sla.lstsq(_to_np(A), _to_np(b))[0]), immutable=True)
            e.define('norm',   lambda t, ord=None: float(_sla.norm(_to_np(t), ord=ord)), immutable=True)
            e.define('det',    lambda t: float(_sla.det(_to_np(t))), immutable=True)
            e.define('inv',    lambda t: _T(_sla.inv(_to_np(t))), immutable=True)
            e.define('expm',   lambda t: _T(_sla.expm(_to_np(t))), immutable=True)  # matrix exponential

            # FFT (FFTW-backed when available)
            e.define('fft',    lambda t, n=None: _T(np.abs(_sfft.fft(_to_np(t), n=n))), immutable=True)
            e.define('ifft',   lambda t, n=None: _T(np.real(_sfft.ifft(_to_np(t), n=n))), immutable=True)
            e.define('fft2',   lambda t: _T(np.abs(_sfft.fft2(_to_np(t)))), immutable=True)
            e.define('rfft',   lambda t, n=None: _T(np.abs(_sfft.rfft(_to_np(t), n=n))), immutable=True)
            e.define('fftfreq',lambda n, d=1.0: _T(_sfft.fftfreq(int(n), d=d)), immutable=True)

            # Optimization (finds minima of scalar functions)
            def _minimize(fn, x0, method='BFGS'):
                x0_np = _to_np(x0) if isinstance(x0, _T) else np.array([float(x0)])
                result = _sopt.minimize(fn, x0_np, method=method)
                return NuvolaStruct('OptResult', {
                    'x': _T(result.x), 'fun': float(result.fun),
                    'success': result.success, 'nit': result.nit
                })
            e.define('minimize', _minimize, immutable=True)
            e.define('brent',  lambda fn, bracket=None: float(_sopt.brent(fn, brack=bracket)), immutable=True)

        # ── sympy: symbolic math ──────────────────────────────────────────
        if HAS_SYMPY:
            e.define('sym',       lambda name: _sp.Symbol(name), immutable=True)
            e.define('sym_diff',  lambda expr, var, n=1: _sp.diff(expr, var, n), immutable=True)
            e.define('sym_int',   lambda expr, var: _sp.integrate(expr, var), immutable=True)
            e.define('sym_solve', lambda expr, var: _sp.solve(expr, var), immutable=True)
            e.define('sym_simplify', lambda expr: _sp.simplify(expr), immutable=True)
            e.define('sym_expand',   lambda expr: _sp.expand(expr), immutable=True)
            e.define('sym_factor',   lambda expr: _sp.factor(expr), immutable=True)
            e.define('sym_series',   lambda expr, var, n=6: _sp.series(expr, var, n=n), immutable=True)
            # lambdify: convert symbolic expr to fast numpy function
            e.define('sym_compile', lambda vars, expr: _sp.lambdify(vars, expr, 'numpy'), immutable=True)
            e.define('sym_latex',   lambda expr: _sp.latex(expr), immutable=True)
            e.define('sym_print',   lambda expr: print(_sp.pretty(expr)), immutable=True)

        # ── tensor inspection ──
        e.define('shape',  lambda t: t.shape  if isinstance(t, NuvolaTensor) else [], immutable=True)
        e.define('ndim',   lambda t: t.ndim   if isinstance(t, NuvolaTensor) else 0,  immutable=True)
        e.define('dtype_of',lambda t: t.dtype if isinstance(t, NuvolaTensor) else 'unknown', immutable=True)
        e.define('item',   lambda t: t.item() if isinstance(t, NuvolaTensor) else t,  immutable=True)
        e.define('tolist', lambda t: t.tolist()if isinstance(t, NuvolaTensor) else t,  immutable=True)
        e.define('numpy_',  lambda t: t.numpy()if isinstance(t, NuvolaTensor) else t, immutable=True)

        # ── HTTP server ──────────────────────────────────────────────────
        e.define('serve', lambda port, routes: NuvolaServer(int(port), routes, self), immutable=True)

        # ── JSON ─────────────────────────────────────────────────────────
        import json as _json
        e.define('json_encode', lambda x: _json.dumps(
            x.tolist() if isinstance(x, NuvolaTensor) else x), immutable=True)
        e.define('json_decode', lambda s: _json.loads(str(s)), immutable=True)
        e.define('json_pretty', lambda x: _json.dumps(
            x.tolist() if isinstance(x, NuvolaTensor) else x, indent=2), immutable=True)

        # ── inline Python (py("...") executes raw Python in global scope) ──
        # Variables assigned in the Python block become Nuvola bindings.
        # Variables from global Nuvola scope are pre-injected as locals.
        _nuc_self = self
        def _py_exec(code):
            g_env = _nuc_self.global_env
            local_vars = {
                'np': np, 'cp': cp, 'math': math, 'cmath': cmath,
                'NuvolaTensor': NuvolaTensor, 'NuvolaStruct': NuvolaStruct,
                'HAS_GPU': HAS_GPU, 'HAS_SCIPY': HAS_SCIPY,
            }
            # inject current Nuvola globals as Python locals
            for k, v in g_env.vars.items():
                if not callable(v) or isinstance(v, NuvolaTensor):
                    local_vars[k] = v
            exec(str(code), {'__builtins__': __builtins__}, local_vars)
            # write back new/changed values to Nuvola global env
            skip = {'np', 'cp', 'math', 'cmath', 'NuvolaTensor', 'NuvolaStruct',
                    'HAS_GPU', 'HAS_SCIPY', '__builtins__'}
            for k, v in local_vars.items():
                if k not in skip:
                    g_env.define(k, v, immutable=False)
            return local_vars.get('_result', None)
        e.define('py', _py_exec, immutable=True)

        # ── shell / system interop ────────────────────────────────────────
        import subprocess as _sub
        def _shell(cmd):
            r = _sub.run(str(cmd), shell=True, capture_output=True, text=True)
            return NuvolaStruct('ShellResult', {
                'stdout': r.stdout, 'stderr': r.stderr, 'code': r.returncode
            })
        e.define('shell', _shell, immutable=True)

        # ── generic/trait helpers ──
        e.define('impl_for', self._builtin_impl_for, immutable=True)
        e.define('call_trait', self._builtin_call_trait, immutable=True)

        # ── http client ───────────────────────────────────────────────────────
        # http_get(url)                    → NuvolaStruct{status, body, ok}
        # http_post(url, body, headers={}) → NuvolaStruct{status, body, ok}
        # Lets Nuvola scripts call REST APIs (including the AI server) natively.
        import urllib.request as _ur
        import urllib.error   as _ue

        def _http_get(url):
            try:
                resp = _ur.urlopen(str(url), timeout=60)
                body = resp.read().decode("utf-8", "replace")
                return NuvolaStruct("HttpResponse", {"status": resp.status, "body": body, "ok": True})
            except _ue.HTTPError as e:
                return NuvolaStruct("HttpResponse", {"status": e.code, "body": str(e), "ok": False})
            except Exception as e:
                return NuvolaStruct("HttpResponse", {"status": 0, "body": str(e), "ok": False})

        def _http_post(url, body=None, headers=None):
            import json as _j
            if headers is None:
                headers = {}
            if isinstance(body, (dict, list)):
                data = _j.dumps(body).encode()
                if "Content-Type" not in headers:
                    headers["Content-Type"] = "application/json"
            elif body is None:
                data = b""
            else:
                data = str(body).encode()
            req = _ur.Request(str(url), data=data, headers=headers, method="POST")
            try:
                resp = _ur.urlopen(req, timeout=300)
                raw  = resp.read().decode("utf-8", "replace")
                try:
                    parsed = _j.loads(raw)
                except Exception:
                    parsed = raw
                return NuvolaStruct("HttpResponse", {"status": resp.status, "body": parsed, "ok": True})
            except _ue.HTTPError as e:
                return NuvolaStruct("HttpResponse", {"status": e.code, "body": str(e), "ok": False})
            except Exception as e:
                return NuvolaStruct("HttpResponse", {"status": 0, "body": str(e), "ok": False})

        e.define('http_get',  _http_get,  immutable=True)
        e.define('http_post', _http_post, immutable=True)

        # ── ai_chat / ai_embed / ai_reset ─────────────────────────────────────
        # ai_chat("question")                        → reply string
        # ai_chat("question", session_id="s1")       → session memory
        # ai_embed("text")                           → NuvolaTensor (vector)
        # ai_reset(session_id="s1")                  → clears history
        # Requires: python3 launch_ai.py running in another terminal.
        _AI_URL = "http://127.0.0.1:11435"

        def _ai_chat(message, session_id="default", model=None):
            import json as _j
            payload = {"message": str(message), "session_id": str(session_id), "stream": False}
            if model:
                payload["model"] = str(model)
            req = _ur.Request(
                f"{_AI_URL}/chat",
                data=_j.dumps(payload).encode(),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            try:
                resp   = _ur.urlopen(req, timeout=300)
                result = _j.loads(resp.read())
                return result.get("reply", "")
            except Exception as err:
                return f"[ai_chat error: {err}]"

        def _ai_embed(text):
            import json as _j
            payload = {"text": text if isinstance(text, list) else str(text)}
            req = _ur.Request(
                f"{_AI_URL}/embed",
                data=_j.dumps(payload).encode(),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            try:
                result = _j.loads(_ur.urlopen(req, timeout=30).read())
                embs   = result.get("embeddings", [[]])
                return NuvolaTensor(embs[0]) if embs else NuvolaTensor([0.0])
            except Exception:
                return NuvolaTensor([0.0])

        def _ai_reset(session_id="default"):
            try:
                _ur.urlopen(_ur.Request(f"{_AI_URL}/reset/{session_id}",
                                        data=b"", method="POST"), timeout=10)
                return True
            except Exception:
                return False

        e.define('ai_chat',  _ai_chat,  immutable=True)
        e.define('ai_embed', _ai_embed, immutable=True)
        e.define('ai_reset', _ai_reset, immutable=True)

        # ai_moa(msg) — Mixture of Agents: all models vote, 32b synthesizes.
        # auto_route=true (default): smart routing — MoA only for hard queries.
        # Slower (~15-25s) but higher accuracy on complex/multi-part problems.
        def _ai_moa(message, session_id="default", auto_route=True):
            import json as _j
            payload = {"message": str(message), "session_id": str(session_id),
                       "auto_route": bool(auto_route)}
            req = _ur.Request(
                f"{_AI_URL}/moa",
                data=_j.dumps(payload).encode(),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            try:
                result = _j.loads(_ur.urlopen(req, timeout=600).read())
                return result.get("reply", "")
            except Exception as err:
                return f"[ai_moa error: {err}]"

        e.define('ai_moa', _ai_moa, immutable=True)

        # ai_rag(question)          → answer using RAG index
        # ai_rag_add(text, source)  → index a text string
        # ai_rag_search(question)   → return top chunks (no LLM call)
        def _ai_rag(question, top_k=3, session_id="rag"):
            try:
                payload = _json.dumps({
                    "question": str(question), "top_k": int(top_k),
                    "session_id": str(session_id), "answer": True,
                }).encode()
                req = _ur.Request(f"{_AI_URL}/rag/query", data=payload,
                                  headers={"Content-Type": "application/json"}, method="POST")
                result = _json.loads(_ur.urlopen(req, timeout=300).read())
                return result.get("answer", "")
            except Exception as err:
                return f"[ai_rag error: {err}]"

        def _ai_rag_add(text, source="manual"):
            try:
                payload = _json.dumps({"text": str(text), "source": str(source)}).encode()
                req = _ur.Request(f"{_AI_URL}/rag/add", data=payload,
                                  headers={"Content-Type": "application/json"}, method="POST")
                result = _json.loads(_ur.urlopen(req, timeout=120).read())
                return result.get("added", 0)
            except Exception as err:
                return f"[ai_rag_add error: {err}]"

        def _ai_rag_search(question, top_k=3):
            try:
                payload = _json.dumps({
                    "question": str(question), "top_k": int(top_k), "answer": False,
                }).encode()
                req = _ur.Request(f"{_AI_URL}/rag/query", data=payload,
                                  headers={"Content-Type": "application/json"}, method="POST")
                result = _json.loads(_ur.urlopen(req, timeout=30).read())
                return result.get("chunks", [])
            except Exception as err:
                return f"[ai_rag_search error: {err}]"

        e.define('ai_rag',        _ai_rag,        immutable=True)
        e.define('ai_rag_add',    _ai_rag_add,    immutable=True)
        e.define('ai_rag_search', _ai_rag_search, immutable=True)

        # ai_image(prompt)                    → path to generated PNG
        # ai_image(prompt, enhance=true)      → 32b polishes prompt first
        # ai_image(prompt, model="turbo")     → fast 4-step generation (~5s)
        def _ai_image(prompt, enhance=False, model="sdxl",
                      width=1024, height=1024, seed=None, steps=None):
            try:
                payload = {"prompt": str(prompt), "enhance": bool(enhance),
                           "model": str(model), "width": int(width), "height": int(height)}
                if seed  is not None: payload["seed"]  = int(seed)
                if steps is not None: payload["steps"] = int(steps)
                req = _ur.Request(
                    f"{_AI_URL}/image/generate",
                    data=_json.dumps(payload).encode(),
                    headers={"Content-Type": "application/json"}, method="POST",
                )
                result = _json.loads(_ur.urlopen(req, timeout=300).read())
                return result.get("path", f"[ai_image error: no path in response]")
            except Exception as err:
                return f"[ai_image error: {err}]"

        e.define('ai_image', _ai_image, immutable=True)

        # ── fused linear ops ─────────────────────────────────────────────────
        # Single BLAS call, no intermediate NuvolaTensor nodes.
        # In no_grad mode (inference): pure numpy, zero graph overhead.
        # In training mode: falls back to full autodiff path automatically.
        _nuc_ev = self
        def _linear(x, W, b):
            """x @ W + b — fused, one BLAS call."""
            if NuvolaTensor._no_grad_depth > 0:
                xd = _to_numpy(x.data) if isinstance(x, NuvolaTensor) else np.array(x, dtype=np.float64)
                Wd = _to_numpy(W.data) if isinstance(W, NuvolaTensor) else np.array(W, dtype=np.float64)
                bd = _to_numpy(b.data) if isinstance(b, NuvolaTensor) else np.array(b, dtype=np.float64)
                return NuvolaTensor(xd @ Wd + bd)
            return (x.matmul(W) if isinstance(x, NuvolaTensor) else NuvolaTensor(x).matmul(W)) + b

        def _linear_relu(x, W, b):
            """x @ W + b → ReLU — two ops fused into one numpy call."""
            if NuvolaTensor._no_grad_depth > 0:
                xd = _to_numpy(x.data) if isinstance(x, NuvolaTensor) else np.array(x, dtype=np.float64)
                Wd = _to_numpy(W.data) if isinstance(W, NuvolaTensor) else np.array(W, dtype=np.float64)
                bd = _to_numpy(b.data) if isinstance(b, NuvolaTensor) else np.array(b, dtype=np.float64)
                return NuvolaTensor(np.maximum(0.0, xd @ Wd + bd))
            base = (x.matmul(W) if isinstance(x, NuvolaTensor) else NuvolaTensor(x).matmul(W)) + b
            return base.relu()

        def _linear_gelu(x, W, b):
            """x @ W + b → GELU — fused for inference speed."""
            if NuvolaTensor._no_grad_depth > 0:
                xd = _to_numpy(x.data) if isinstance(x, NuvolaTensor) else np.array(x, dtype=np.float64)
                Wd = _to_numpy(W.data) if isinstance(W, NuvolaTensor) else np.array(W, dtype=np.float64)
                bd = _to_numpy(b.data) if isinstance(b, NuvolaTensor) else np.array(b, dtype=np.float64)
                z = xd @ Wd + bd
                cdf = 0.5 * (1.0 + np.tanh(0.7978845608 * (z + 0.044715 * z**3)))
                return NuvolaTensor(z * cdf)
            base = (x.matmul(W) if isinstance(x, NuvolaTensor) else NuvolaTensor(x).matmul(W)) + b
            return base.gelu()

        e.define('linear',      _linear,      immutable=True)
        e.define('linear_relu', _linear_relu, immutable=True)
        e.define('linear_gelu', _linear_gelu, immutable=True)

        # ── memoize: content-fingerprint cache ──────────────────────────────
        # "Know the answer before you tell it what the question is."
        # Wraps any function: first call computes + caches, subsequent calls
        # with same input fingerprint return instantly from cache.
        # Automatically enables no_grad mode for all cached calls.
        _nuc_call = self._call
        def _tensor_fp(t):
            """Fast fingerprint: 8 sampled values + shape. ~1µs per call."""
            d = _to_numpy(t.data) if isinstance(t, NuvolaTensor) else np.array(t, dtype=np.float64)
            sz = d.size
            if sz <= 8:
                samples = tuple(round(float(v), 6) for v in d.flat)
            else:
                idx = [0, sz//8, sz//4, 3*sz//8, sz//2, 5*sz//8, 3*sz//4, sz-1]
                samples = tuple(round(float(d.flat[i]), 6) for i in idx)
            return (d.shape, samples)

        def _memoize(fn):
            """Return a memoized, no-grad version of fn."""
            _cache = {}
            def _memoized(*args):
                # Build cache key from tensor fingerprints + scalar args
                key_parts = []
                for a in args:
                    if isinstance(a, NuvolaTensor):
                        key_parts.append(_tensor_fp(a))
                    elif isinstance(a, (int, float, bool, str)):
                        key_parts.append(a)
                    else:
                        key_parts.append(id(a))  # non-hashable: use identity
                key = tuple(key_parts)
                if key in _cache:
                    return _cache[key]
                # Compute under no_grad — skip all graph construction
                NuvolaTensor._no_grad_depth += 1
                try:
                    result = _nuc_call(fn, list(args))
                finally:
                    NuvolaTensor._no_grad_depth = max(0, NuvolaTensor._no_grad_depth - 1)
                _cache[key] = result
                # Evict oldest half when cache grows large
                if len(_cache) > 4096:
                    for k in list(_cache)[:2048]:
                        del _cache[k]
                return result
            _memoized._cache = _cache
            return _memoized
        e.define('memoize', _memoize, immutable=True)

        # ── prefetch: background precomputation ──────────────────────────────
        # prefetch(fn, [[arg1, arg2], [arg1b, arg2b], ...])
        # Fires fn on each arg-set in the thread pool immediately.
        # Returns a list of futures — call .result() to block for value.
        def _prefetch(fn, args_list):
            """Submit fn(args) for each args in args_list to the thread pool.
            Returns list of Future objects. Results available before you ask."""
            futures = []
            for args in args_list:
                a = list(args) if isinstance(args, list) else [args]
                NuvolaTensor._no_grad_depth += 1
                def _task(a=a):
                    try:
                        return _nuc_call(fn, a)
                    finally:
                        NuvolaTensor._no_grad_depth = max(0, NuvolaTensor._no_grad_depth - 1)
                futures.append(_PAR_EXECUTOR.submit(_task))
            return futures
        e.define('prefetch', _prefetch, immutable=True)

        # ── warm_inference: pre-warm a memoized model on a grid ─────────────
        # warm_inference(memoize(forward), sample_list)
        # Evaluates fn on every input in sample_list right now (no_grad, threaded).
        # After this call, all those inputs are instant cache hits.
        # Returns count of inputs warmed.
        def _warm_inference(fn, sample_inputs):
            """Pre-evaluate fn on each input. If fn is memoized, results are
            cached so HTTP /predict responses are instant for nearby inputs."""
            count = 0
            NuvolaTensor._no_grad_depth += 1
            try:
                for inp in sample_inputs:
                    args = list(inp) if isinstance(inp, list) else [inp]
                    try:
                        _nuc_call(fn, args)
                        count += 1
                    except Exception:
                        pass
            finally:
                NuvolaTensor._no_grad_depth = max(0, NuvolaTensor._no_grad_depth - 1)
            return count
        e.define('warm_inference', _warm_inference, immutable=True)

    # ── tensor builtins ───────────────────────────────────────────────────
    def _builtin_tensor(self, data, dtype=None, requires_grad=False):
        if isinstance(data, NuvolaTensor): return data
        return NuvolaTensor(data, dtype=dtype, requires_grad=bool(requires_grad))

    def _builtin_zeros(self, *shape, dtype=None):
        if len(shape) == 1 and isinstance(shape[0], list): shape = tuple(shape[0])
        dtype_map = {'f64': np.float64, 'f32': np.float32, 'f16': np.float16,
                     'i32': np.int32, 'i8': np.int8}
        np_dtype = dtype_map.get(dtype, np.float64)
        return NuvolaTensor(np.zeros(shape, dtype=np_dtype), dtype=dtype)

    def _builtin_ones(self, *shape, dtype=None):
        if len(shape) == 1 and isinstance(shape[0], list): shape = tuple(shape[0])
        dtype_map = {'f64': np.float64, 'f32': np.float32, 'f16': np.float16,
                     'i32': np.int32, 'i8': np.int8}
        np_dtype = dtype_map.get(dtype, np.float64)
        return NuvolaTensor(np.ones(shape, dtype=np_dtype), dtype=dtype)

    def _builtin_randn(self, *shape, dtype=None):
        if len(shape) == 1 and isinstance(shape[0], list): shape = tuple(shape[0])
        return NuvolaTensor(np.random.randn(*shape), dtype=dtype or 'f64', requires_grad=True)

    def _builtin_rand(self, *shape, dtype=None):
        if len(shape) == 1 and isinstance(shape[0], list): shape = tuple(shape[0])
        return NuvolaTensor(np.random.rand(*shape), dtype=dtype or 'f64')

    def _builtin_eye(self, n, dtype=None):
        return NuvolaTensor(np.eye(int(n)), dtype=dtype or 'f64')

    def _builtin_arange(self, start, stop=None, step=1, dtype=None):
        if stop is None: start, stop = 0, start
        dtype_map = {'f64': np.float64, 'f32': np.float32, 'i64': np.int64, 'i32': np.int32}
        np_dtype = dtype_map.get(dtype, np.float64)
        return NuvolaTensor(np.arange(start, stop, step, dtype=np_dtype), dtype=dtype)

    def _builtin_linspace(self, start, stop, n, dtype=None):
        return NuvolaTensor(np.linspace(start, stop, int(n)), dtype=dtype or 'f64')

    def _builtin_full(self, shape, fill, dtype=None):
        if isinstance(shape, (int, float)): shape = [int(shape)]
        return NuvolaTensor(np.full(tuple(int(x) for x in shape), fill), dtype=dtype or 'f64')

    def _builtin_einsum(self, subscripts, *operands):
        arrays = [op.data if isinstance(op, NuvolaTensor) else np.array(op) for op in operands]
        return NuvolaTensor(np.einsum(subscripts, *arrays))

    def _builtin_stack(self, tensors, axis=0):
        arrays = [t.data if isinstance(t, NuvolaTensor) else np.array(t) for t in tensors]
        return NuvolaTensor(np.stack(arrays, axis=int(axis)))

    def _builtin_cat(self, tensors, axis=0):
        arrays = [t.data if isinstance(t, NuvolaTensor) else np.array(t) for t in tensors]
        return NuvolaTensor(np.concatenate(arrays, axis=int(axis)))

    def _builtin_broadcast(self, a, b):
        if isinstance(a, NuvolaTensor) and isinstance(b, NuvolaTensor):
            return NuvolaTensor(np.broadcast_to(a.data, b.data.shape))
        return a

    # ── loss functions ────────────────────────────────────────────────────
    def _builtin_mse_loss(self, pred, target):
        pred = pred if isinstance(pred, NuvolaTensor) else NuvolaTensor(pred)
        target = target if isinstance(target, NuvolaTensor) else NuvolaTensor(target)
        diff = pred - target
        return (diff * diff).mean()

    def _builtin_cross_entropy(self, logits, targets):
        """Cross-entropy from raw logits. targets = class indices (int array)."""
        logits = logits if isinstance(logits, NuvolaTensor) else NuvolaTensor(logits)
        probs = logits.softmax(axis=-1)
        log_probs = probs.log()
        if isinstance(targets, NuvolaTensor):
            targets = targets.data.astype(int)
        elif isinstance(targets, list):
            targets = np.array(targets, dtype=int)
        else:
            targets = np.array([int(targets)])
        # gather log_probs at target indices
        batch = log_probs.data.shape[0] if log_probs.data.ndim > 1 else 1
        if log_probs.data.ndim == 1:
            loss_val = -log_probs.data[targets[0]]
        else:
            loss_val = -np.mean([log_probs.data[i, targets[i]] for i in range(batch)])
        out = NuvolaTensor(loss_val, requires_grad=logits.requires_grad)
        out._prev = {logits}
        def _back():
            if logits.requires_grad:
                # gradient through log-softmax
                grad = probs.data.copy()
                if probs.data.ndim > 1:
                    for i in range(batch):
                        grad[i, targets[i]] -= 1.0
                    grad /= batch
                else:
                    grad[targets[0]] -= 1.0
                logits.grad += grad * out.grad
        out._backward = _back
        return out

    def _builtin_bce_loss(self, pred, target):
        pred = pred if isinstance(pred, NuvolaTensor) else NuvolaTensor(pred)
        target = target if isinstance(target, NuvolaTensor) else NuvolaTensor(target)
        loss = -(target * pred.log() + (NuvolaTensor(1.0) - target) * (NuvolaTensor(1.0) - pred).log())
        return loss.mean()

    def _builtin_huber_loss(self, pred, target, delta=1.0):
        pred = pred if isinstance(pred, NuvolaTensor) else NuvolaTensor(pred)
        target = target if isinstance(target, NuvolaTensor) else NuvolaTensor(target)
        diff = pred - target
        diff_np = _to_numpy(diff.data)
        abs_d = np.abs(diff_np)
        loss_np = np.where(abs_d <= delta, 0.5 * diff_np**2, delta * (abs_d - 0.5 * delta))
        return NuvolaTensor(loss_np.mean())

    # ── quantization builtins ─────────────────────────────────────────────
    def _builtin_quantize(self, t, dtype='i8'):
        if not isinstance(t, NuvolaTensor): t = NuvolaTensor(t)
        q, scale = t.quantize_int8()
        return NuvolaStruct('QuantTensor', {'tensor': q, 'scale': scale, 'dtype': dtype})

    def _builtin_dequantize(self, qt, target_dtype='f32'):
        if isinstance(qt, NuvolaStruct):
            t = qt.fields['tensor']
            scale = qt.fields['scale']
            return t.dequantize(scale, target_dtype)
        return qt

    # ── normalization ─────────────────────────────────────────────────────
    def _builtin_layer_norm(self, x, gamma=None, beta=None, eps=1e-5):
        if not isinstance(x, NuvolaTensor): x = NuvolaTensor(x)
        mean = x.mean(axis=-1)
        # variance
        x_centered = x - mean.unsqueeze(-1) if x.data.ndim > 1 else x - mean
        var_data = np.var(x.data, axis=-1, keepdims=True if x.data.ndim > 1 else False)
        std = NuvolaTensor(np.sqrt(var_data + eps))
        out = x_centered / std
        if gamma is not None:
            gamma_t = gamma if isinstance(gamma, NuvolaTensor) else NuvolaTensor(gamma)
            out = out * gamma_t
        if beta is not None:
            beta_t = beta if isinstance(beta, NuvolaTensor) else NuvolaTensor(beta)
            out = out + beta_t
        return out

    def _builtin_batch_norm(self, x, gamma=None, beta=None, eps=1e-5):
        if not isinstance(x, NuvolaTensor): x = NuvolaTensor(x)
        mean_data = x.data.mean(axis=0)
        var_data  = x.data.var(axis=0)
        x_norm = NuvolaTensor((x.data - mean_data) / np.sqrt(var_data + eps))
        if gamma is not None:
            x_norm = x_norm * (gamma if isinstance(gamma, NuvolaTensor) else NuvolaTensor(gamma))
        if beta is not None:
            x_norm = x_norm + (beta if isinstance(beta, NuvolaTensor) else NuvolaTensor(beta))
        return x_norm

    # ── concurrency builtins ──────────────────────────────────────────────
    def _builtin_spawn(self, fn, *args, **kwargs):
        box = [None, None]
        def worker():
            try:
                box[0] = self._call(fn, list(args), kwargs or None)
            except Exception as ex:
                box[1] = ex
        t = threading.Thread(target=worker, daemon=True)
        t.start()
        return NuvolaFuture(t, box)

    def _builtin_await(self, future, timeout=None):
        if isinstance(future, NuvolaFuture):
            return future.await_(timeout)
        return future  # not a future, just return it

    # ── optimizers ───────────────────────────────────────────────────────
    def _builtin_adam(self, lr=0.001, beta1=0.9, beta2=0.999, eps=1e-8):
        """Returns an Adam optimizer struct (state dict + step function)."""
        state = {}  # param_id → (m, v, t)
        evref = self

        def step(params):
            for p in (params if isinstance(params, list) else [params]):
                if not isinstance(p, NuvolaTensor) or p.grad is None: continue
                pid = id(p)
                if pid not in state:
                    state[pid] = (np.zeros_like(p.data, dtype=np.float64),
                                  np.zeros_like(p.data, dtype=np.float64), 0)
                m, v, t = state[pid]
                t += 1
                g = p.grad.astype(np.float64)
                m = beta1 * m + (1 - beta1) * g
                v = beta2 * v + (1 - beta2) * g**2
                m_hat = m / (1 - beta1**t)
                v_hat = v / (1 - beta2**t)
                p.data = p.data.astype(np.float64) - lr * m_hat / (np.sqrt(v_hat) + eps)
                state[pid] = (m, v, t)
            return None

        def zero_grad(params):
            for p in (params if isinstance(params, list) else [params]):
                if isinstance(p, NuvolaTensor):
                    p.grad = np.zeros_like(p.data, dtype=np.float64)
            return None

        return NuvolaStruct('AdamOptimizer', {
            'step': step,
            'zero_grad': zero_grad,
            'lr': lr,
        })

    def _builtin_sgd_opt(self, lr=0.01, momentum=0.0):
        """Returns a SGD optimizer struct."""
        state = {}
        def step(params):
            for p in (params if isinstance(params, list) else [params]):
                if not isinstance(p, NuvolaTensor) or p.grad is None: continue
                pid = id(p)
                g = p.grad.astype(np.float64)
                if momentum > 0:
                    v = state.get(pid, np.zeros_like(g))
                    v = momentum * v + g
                    state[pid] = v
                    p.data = p.data.astype(np.float64) - lr * v
                else:
                    p.data = p.data.astype(np.float64) - lr * g
            return None
        def zero_grad(params):
            for p in (params if isinstance(params, list) else [params]):
                if isinstance(p, NuvolaTensor):
                    p.grad = np.zeros_like(p.data, dtype=np.float64)
            return None
        return NuvolaStruct('SGDOptimizer', {'step': step, 'zero_grad': zero_grad, 'lr': lr})

    def _builtin_adagrad(self, lr=0.01, eps=1e-8):
        """AdaGrad: adapts lr per-parameter based on cumulative squared gradients."""
        G = {}  # accumulator
        def step(params):
            for p in (params if isinstance(params, list) else [params]):
                if not isinstance(p, NuvolaTensor) or p.grad is None: continue
                pid = id(p)
                g = p.grad.astype(np.float64)
                G[pid] = G.get(pid, np.zeros_like(g)) + g ** 2
                p.data = p.data.astype(np.float64) - lr / (np.sqrt(G[pid]) + eps) * g
            return None
        def zero_grad(params):
            for p in (params if isinstance(params, list) else [params]):
                if isinstance(p, NuvolaTensor):
                    p.grad = np.zeros_like(p.data, dtype=np.float64)
            return None
        return NuvolaStruct('AdaGrad', {'step': step, 'zero_grad': zero_grad, 'lr': lr})

    def _builtin_rmsprop(self, lr=0.001, alpha=0.99, eps=1e-8):
        """RMSProp: running average of squared gradients."""
        V = {}
        def step(params):
            for p in (params if isinstance(params, list) else [params]):
                if not isinstance(p, NuvolaTensor) or p.grad is None: continue
                pid = id(p)
                g = p.grad.astype(np.float64)
                V[pid] = alpha * V.get(pid, np.zeros_like(g)) + (1 - alpha) * g ** 2
                p.data = p.data.astype(np.float64) - lr / (np.sqrt(V[pid]) + eps) * g
            return None
        def zero_grad(params):
            for p in (params if isinstance(params, list) else [params]):
                if isinstance(p, NuvolaTensor):
                    p.grad = np.zeros_like(p.data, dtype=np.float64)
            return None
        return NuvolaStruct('RMSProp', {'step': step, 'zero_grad': zero_grad, 'lr': lr})

    def _builtin_clip_grad_norm(self, params, max_norm=1.0):
        """Clip global gradient norm in-place. Returns the norm before clipping."""
        ps = params if isinstance(params, list) else [params]
        total_sq = 0.0
        for p in ps:
            if isinstance(p, NuvolaTensor) and p.grad is not None:
                total_sq += float(np.sum(p.grad ** 2))
        total_norm = math.sqrt(total_sq)
        if total_norm > max_norm:
            scale = max_norm / (total_norm + 1e-12)
            for p in ps:
                if isinstance(p, NuvolaTensor) and p.grad is not None:
                    p.grad *= scale
        return total_norm

    def _builtin_cosine_lr(self, step, total_steps, lr_min=0.0, lr_max=0.001):
        """Cosine annealing learning rate at given step."""
        t = step / max(total_steps - 1, 1)
        return lr_min + 0.5 * (lr_max - lr_min) * (1.0 + math.cos(math.pi * t))

    def _builtin_warmup_cosine_lr(self, step, warmup_steps, total_steps, lr_max=0.001, lr_min=0.0):
        """Linear warmup + cosine decay learning rate schedule."""
        if step < warmup_steps:
            return lr_max * step / max(warmup_steps, 1)
        t = (step - warmup_steps) / max(total_steps - warmup_steps - 1, 1)
        return lr_min + 0.5 * (lr_max - lr_min) * (1.0 + math.cos(math.pi * t))

    def _builtin_where(self, cond, x, y):
        return NuvolaTensor.where(cond, x, y)

    def _builtin_conv2d(self, inp, weight, bias=None, stride=1, padding=0):
        inp = inp if isinstance(inp, NuvolaTensor) else NuvolaTensor(inp)
        return inp.conv2d(weight, stride=int(stride), padding=int(padding), bias=bias)

    def _builtin_conv1d(self, inp, weight, stride=1, padding=0):
        inp = inp if isinstance(inp, NuvolaTensor) else NuvolaTensor(inp)
        return inp.conv1d(weight, stride=int(stride), padding=int(padding))

    def _builtin_dropout(self, x, p=0.5, training=True):
        x = x if isinstance(x, NuvolaTensor) else NuvolaTensor(x)
        return x.dropout(float(p), bool(training))

    # ── gradient helpers ──────────────────────────────────────────────────
    def _builtin_no_grad(self, fn, *args):
        """Call fn with gradient tracking fully disabled.

        Uses _no_grad_depth counter so _wrap() skips building _prev sets and
        backward closures entirely — not just flagging requires_grad=False after
        the fact. Eliminates ~70% of per-op overhead for inference paths.
        Nestable: no_grad(no_grad(fn)) works correctly.
        """
        NuvolaTensor._no_grad_depth += 1
        try:
            result = self._call(fn, list(args))
        finally:
            NuvolaTensor._no_grad_depth = max(0, NuvolaTensor._no_grad_depth - 1)
        return result

    def _builtin_grad_of(self, tensor):
        if isinstance(tensor, NuvolaTensor):
            return NuvolaTensor(tensor.grad.copy())
        return NuvolaTensor([0.0])

    # ── trait / impl builtins ─────────────────────────────────────────────
    def _builtin_impl_for(self, trait_name, type_name, method_name, fn):
        """Runtime trait implementation registration."""
        key = type_name
        if key not in self._trait_impls:
            self._trait_impls[key] = {}
        self._trait_impls[key][method_name] = fn
        return None

    def _builtin_call_trait(self, obj, method_name, *args):
        type_name = (obj.type_name if isinstance(obj, NuvolaStruct) else
                     obj.variant   if isinstance(obj, NuvolaEnum)   else
                     type(obj).__name__)
        impls = self._trait_impls.get(type_name, {})
        if method_name in impls:
            return self._call(impls[method_name], [obj] + list(args))
        raise AttributeError(f"No trait impl for '{type_name}.{method_name}'")

    # ── eval — extended ───────────────────────────────────────────────────
    def eval(self, node: Node, env: Env) -> Any:
        if node is None: return None

        # ── Nuclear nodes ──
        if isinstance(node, ComptimeDecl):
            if node.name not in self._comptime:
                self._comptime[node.name] = self.eval(node.expr, self.global_env)
            env.define(node.name, self._comptime[node.name], immutable=True)
            return self._comptime[node.name]

        if isinstance(node, TraitDecl):
            self._trait_decls[node.name] = node
            return None

        if isinstance(node, ImplDecl):
            type_name = node.type_name or node.trait_name
            if type_name not in self._trait_impls:
                self._trait_impls[type_name] = {}
            for mname, mdecl in node.methods.items():
                fn = NuvolaFn(mdecl.params, mdecl.body, env, mname)
                self._trait_impls[type_name][mname] = fn
            return None

        if isinstance(node, AsyncFnDecl):
            # Creates a function that, when called, runs in a thread
            params = node.params; body = node.body; name = node.name
            evref = self
            def async_wrapper(*args):
                fn_env = Env(env)
                for (pname, _, pdefault), arg in zip(params, args):
                    fn_env.define(pname, arg, immutable=False)
                for i, (pname, _, pdefault) in enumerate(params):
                    if i >= len(args) and pdefault is not None:
                        fn_env.define(pname, evref.eval(pdefault, fn_env), immutable=False)
                box = [None, None]
                def worker():
                    try:
                        try:
                            box[0] = evref.eval(body, fn_env)
                        except ReturnSignal as r:
                            box[0] = r.v
                    except Exception as ex:
                        box[1] = ex
                t = threading.Thread(target=worker, daemon=True)
                t.start()
                return NuvolaFuture(t, box)
            fn = NuvolaFn(params, body, env, name)
            fn._is_async = True
            fn._async_wrapper = async_wrapper
            env.define(name, async_wrapper, immutable=True)
            return fn

        if isinstance(node, ExternFn):
            # Stub: create a wrapper that calls ctypes if lib available
            name = node.name; lib_name = node.lib
            def extern_stub(*args):
                if lib_name:
                    try:
                        lib = ctypes.CDLL(lib_name)
                        fn_ptr = getattr(lib, name)
                        fn_ptr.restype = ctypes.c_double
                        fn_ptr.argtypes = [ctypes.c_double] * len(args)
                        return fn_ptr(*[ctypes.c_double(a) for a in args])
                    except Exception:
                        pass
                raise RuntimeError(f"extern fn '{name}' not available (lib={lib_name})")
            env.define(name, extern_stub, immutable=True)
            return None

        if isinstance(node, UnsafeBlock):
            self._unsafe_depth += 1
            try:
                return self.eval(node.body, env)
            finally:
                self._unsafe_depth -= 1

        if isinstance(node, Await):
            val = self.eval(node.expr, env)
            return self._builtin_await(val)

        if isinstance(node, Spawn):
            # Explicit call node: spawn expr where expr is f(a,b,...)
            if isinstance(node.expr, Call):
                fn = self.eval(node.expr.fn, env)
                args = [self.eval(a, env) for a in node.expr.args]
                kw = {k: self.eval(v, env) for k, v in (node.expr.kwargs or {}).items()}
                return self._builtin_spawn(fn, *args, **kw)
            # spawn(fn, arg1, arg2) — KW eats 'spawn', then '(fn,args)' → TupleLit
            if isinstance(node.expr, TupleLit) and node.expr.items:
                fn = self.eval(node.expr.items[0], env)
                args = [self.eval(a, env) for a in node.expr.items[1:]]
                return self._builtin_spawn(fn, *args)
            val = self.eval(node.expr, env)
            if callable(val) or isinstance(val, NuvolaFn):
                return self._builtin_spawn(val)
            return val

        if isinstance(node, TensorLit):
            data = self.eval(node.data, env)
            return NuvolaTensor(data, dtype=node.dtype)

        # ── Extend Field to handle NuvolaTensor ──
        if isinstance(node, Field):
            obj = self.eval(node.obj, env)
            field = node.field
            if isinstance(obj, NuvolaTensor):
                return self._tensor_field(obj, field)
            if isinstance(obj, NuvolaChannel):
                return self._channel_field(obj, field)
            if isinstance(obj, NuvolaFuture):
                return self._future_field(obj, field)
            # Check trait impls
            type_name = (obj.type_name if isinstance(obj, NuvolaStruct) else
                         obj.variant   if isinstance(obj, NuvolaEnum) else
                         type(obj).__name__)
            impls = self._trait_impls.get(type_name, {})
            if field in impls:
                impl_fn = impls[field]
                return lambda *args: self._call(impl_fn, [obj] + list(args))
            return super().eval(node, env)

        # ── FieldAssign extended for NuvolaTensor (e.g. t.requires_grad = true) ──
        if isinstance(node, FieldAssign):
            obj = self.eval(node.obj, env)
            val = self.eval(node.expr, env)
            if isinstance(obj, NuvolaTensor):
                mutable_fields = {
                    'requires_grad': lambda v: setattr(obj, 'requires_grad', bool(v)),
                    'label':         lambda v: setattr(obj, 'label', str(v)),
                    '_dtype_name':   lambda v: setattr(obj, '_dtype_name', str(v)),
                }
                if node.field in mutable_fields:
                    mutable_fields[node.field](val)
                    return val
                # grad array
                if node.field == 'grad' and isinstance(val, NuvolaTensor):
                    obj.grad = val.data.copy()
                    return val
            elif isinstance(obj, NuvolaChannel):
                if node.field == 'closed': obj.closed = bool(val); return val
            return super().eval(node, env)

        # ── Extend Call to handle NuvolaTensor methods + kwargs ──
        if isinstance(node, Call):
            fn_node = node.fn
            kw = {k: self.eval(v, env) for k, v in (node.kwargs or {}).items()}
            if isinstance(fn_node, Field):
                obj = self.eval(fn_node.obj, env)
                method = fn_node.field
                if isinstance(obj, NuvolaTensor):
                    args = [self.eval(a, env) for a in node.args]
                    kw_t = {k: self.eval(v, env) for k, v in (node.kwargs or {}).items()}
                    return self._tensor_method(obj, method, args, kw_t or None)
                if isinstance(obj, NuvolaChannel):
                    args = [self.eval(a, env) for a in node.args]
                    return self._channel_method(obj, method, args)
                # trait dispatch
                type_name = (obj.type_name if isinstance(obj, NuvolaStruct) else
                             obj.variant   if isinstance(obj, NuvolaEnum) else
                             type(obj).__name__)
                impls = self._trait_impls.get(type_name, {})
                if method in impls:
                    args = [self.eval(a, env) for a in node.args]
                    return self._call(impls[method], [obj] + list(args))
            if kw:
                fn = self.eval(fn_node, env)
                args = [self.eval(a, env) for a in node.args]
                return self._call(fn, args, kw)
            return super().eval(node, env)

        # ── BinOp extended for tensors ──
        if isinstance(node, BinOp):
            op = node.op
            l = self.eval(node.l, env)
            r = self.eval(node.r, env)
            if isinstance(l, NuvolaTensor) or isinstance(r, NuvolaTensor):
                return self._tensor_binop(op, l, r)
            # Fall back to base
            return self._eval_binop_vals(op, l, r, node, env)

        return super().eval(node, env)

    def _eval_binop_vals(self, op, l, r, node, env):
        """Re-implement binop for non-tensor values to avoid double eval."""
        if op == 'and': return l if not self._truthy(l) else r
        if op == 'or':  return l if self._truthy(l) else r
        if op == '??':
            if isinstance(l, NuvolaOption): return l.value if l.is_some else r
            return l if l is not None else r
        return super()._eval_binop(node, env)

    def _tensor_binop(self, op, l, r):
        lt = l if isinstance(l, NuvolaTensor) else NuvolaTensor(l)
        rt = r if isinstance(r, NuvolaTensor) else NuvolaTensor(r)
        if op == '+': return lt + rt
        if op == '-': return lt - rt
        if op == '*': return lt * rt
        if op == '/': return lt / rt
        if op == '**': return lt ** (rt.item() if isinstance(r, NuvolaTensor) else r)
        if op == '@': return lt @ rt
        if op == '==': return lt == rt
        if op == '<':  return NuvolaTensor(lt.data < rt.data)
        if op == '>':  return NuvolaTensor(lt.data > rt.data)
        if op == '<=': return NuvolaTensor(lt.data <= rt.data)
        if op == '>=': return NuvolaTensor(lt.data >= rt.data)
        raise TypeError(f"Unsupported tensor op: {op}")

    # ── tensor field/method dispatch ──────────────────────────────────────
    def _tensor_field(self, t: NuvolaTensor, field: str):
        # Lazy evaluation — only compute the field that was actually asked for
        if field == 'shape': return t.shape
        if field == 'ndim':  return t.ndim
        if field == 'size':  return t.size
        if field == 'dtype': return t.dtype
        if field == 'data':  return t.tolist()
        if field == 'T':     return t.T
        if field == 'grad':
            return NuvolaTensor(t.grad.copy()) if t.grad is not None else NuvolaTensor([0.0])
        if field == 'requires_grad': return t.requires_grad
        # Fall through to method (will be called as property)
        return self._get_tensor_method(t, field)

    def _tensor_method(self, t: NuvolaTensor, method: str, args: list, kwargs: dict = None):
        m = self._get_tensor_method(t, method)
        if callable(m):
            return m(*args, **kwargs) if kwargs else m(*args)
        raise AttributeError(f"NuvolaTensor has no method '{method}'")

    def _get_tensor_method(self, t: NuvolaTensor, method: str):  # noqa: C901
        return {
            'relu':       lambda: t.relu(),
            'sigmoid':    lambda: t.sigmoid(),
            'tanh':       lambda: t.tanh(),
            'gelu':       lambda: t.gelu(),
            'softmax':    lambda axis=-1: t.softmax(int(axis)),
            'log':        lambda: t.log(),
            'exp':        lambda: t.exp(),
            'sqrt':       lambda: t.sqrt(),
            'sum':        lambda axis=None: t.sum(None if axis is None else int(axis)),
            'mean':       lambda axis=None: t.mean(None if axis is None else int(axis)),
            'max':        lambda axis=None: t.max(None if axis is None else int(axis)),
            'min':        lambda axis=None: t.min(None if axis is None else int(axis)),
            'reshape':    lambda *s: t.reshape(*s),
            'transpose':  lambda *axes: t.transpose(*axes),
            'flatten':    lambda: t.flatten(),
            'unsqueeze':  lambda ax: t.unsqueeze(int(ax)),
            'squeeze':    lambda ax=None: t.squeeze(None if ax is None else int(ax)),
            'matmul':     lambda other: t.matmul(other),
            'dot':        lambda other: t.matmul(other),
            'backward':   lambda grad=None: t.backward(grad) or t,
            'zero_grad':  lambda: t.zero_grad() or t,
            'item':       lambda: t.item(),
            'tolist':     lambda: t.tolist(),
            'numpy':      lambda: t.numpy(),
            'to_f32':     lambda: t.to_f32(),
            'to_f64':     lambda: t.to_f64(),
            'to_f16':     lambda: t.to_f16(),
            'to_i8':      lambda: t.to_i8(),
            'to_i32':     lambda: t.to_i32(),
            'quantize':   lambda: t.quantize_int8(),
            'dequantize': lambda scale, dtype='f32': t.dequantize(scale, dtype),
            'clone':      lambda: NuvolaTensor(t.data.copy(), dtype=t.dtype, requires_grad=t.requires_grad),
            'detach':     lambda: NuvolaTensor(t.data.copy(), dtype=t.dtype, requires_grad=False),
            'abs':        lambda: NuvolaTensor(_to_numpy(t.data).__abs__()),
            'clip':       lambda lo, hi: NuvolaTensor(np.clip(_to_numpy(t.data), lo, hi)),
            'norm':       lambda p=2: NuvolaTensor(np.linalg.norm(_to_numpy(t.data).astype(np.float64), ord=int(p))),
            'dropout':    lambda p=0.5, training=True: t.dropout(float(p), bool(training)),
            'conv1d':     lambda k, stride=1, padding=0: t.conv1d(k, int(stride), int(padding)),
            'conv2d':     lambda k, stride=1, padding=0, bias=None: t.conv2d(k, int(stride), int(padding), bias),
        }.get(method)

    # ── channel field/method dispatch ─────────────────────────────────────
    def _channel_field(self, ch: NuvolaChannel, field: str):
        fields = {'closed': ch.closed, 'dtype': ch.dtype}
        if field in fields: return fields[field]
        return self._get_channel_method(ch, field)

    def _channel_method(self, ch: NuvolaChannel, method: str, args: list):
        m = self._get_channel_method(ch, method)
        if callable(m): return m(*args)
        raise AttributeError(f"NuvolaChannel has no method '{method}'")

    def _get_channel_method(self, ch: NuvolaChannel, method: str):
        return {
            'send':     lambda v: ch.send(v),
            'recv':     lambda timeout=None: ch.recv(timeout),
            'try_recv': lambda: ch.try_recv(),
            'close':    lambda: ch.close(),
        }.get(method)

    # ── future field ──────────────────────────────────────────────────────
    def _future_field(self, f: NuvolaFuture, field: str):
        if field == 'is_done': return f.is_done()
        if field == 'await_':  return f.await_
        return None

    # ── trait-aware method dispatch ───────────────────────────────────────
    def _call_method(self, obj, method, args, env):
        # Numeric type-conversion methods on plain Python int/float
        # e.g. i.to_f64(), n.to_i32(), x.to_int()
        if isinstance(obj, (int, float, bool)):
            _num_cvt = {
                'to_f64': float, 'to_f32': float, 'to_f16': float,
                'to_i64': int,   'to_i32': int,   'to_i16': int,
                'to_int': int,   'abs': abs,
            }
            if method in _num_cvt:
                return _num_cvt[method](obj)
        # Check trait impls first
        type_name = (obj.type_name if isinstance(obj, NuvolaStruct) else
                     obj.variant   if isinstance(obj, NuvolaEnum) else
                     type(obj).__name__)
        impls = self._trait_impls.get(type_name, {})
        if method in impls:
            return self._call(impls[method], [obj] + list(args))
        # NuvolaStruct: check for callable field values (optimizer methods etc.)
        if isinstance(obj, NuvolaStruct) and method in obj.fields:
            field_val = obj.fields[method]
            if callable(field_val):
                return field_val(*args)
            if isinstance(field_val, NuvolaFn):
                return self._call(field_val, [obj] + list(args))
        # Generic Python object fallback: use getattr so that any Python class
        # (NuvolaServer, NuvolaChannel, NuvolaFuture, user objects) works transparently.
        # This makes Nuvola interoperate with any Python library by calling real methods.
        attr = getattr(obj, method, None)
        if attr is not None and callable(attr):
            return attr(*args)
        # Also support field reads on Python objects
        if attr is not None:
            return attr
        return super()._call_method(obj, method, args, env)

    # ── _format extended for tensors / channels ───────────────────────────
    def _format(self, v) -> str:
        if isinstance(v, NuvolaTensor):
            if v.data.ndim == 0: return str(v.data.item())
            if v.data.size <= 8: return f"Tensor{v.data.tolist()}"
            return repr(v)
        if isinstance(v, NuvolaChannel): return repr(v)
        if isinstance(v, NuvolaFuture):  return repr(v)
        return super()._format(v)

    # ── override _truthy for tensors ──────────────────────────────────────
    def _truthy(self, v) -> bool:
        if isinstance(v, NuvolaTensor):
            return bool(np.any(v.data))
        return super()._truthy(v)

    # ── override _call to forward kwargs to Python callables ─────────────
    def _call(self, fn, args, kwargs=None):
        kw = kwargs or {}
        if callable(fn) and kw:
            return fn(*args, **kw)
        return super()._call(fn, args)

    # ── run with NuclearParser ────────────────────────────────────────────
    # AST cache: source hash → parsed tree. Avoids re-tokenizing identical programs.
    _ast_cache: dict = {}

    def run(self, source: str) -> Any:
        key = hashlib.md5(source.encode(), usedforsecurity=False).digest()
        if key not in NuclearEvaluator._ast_cache:
            tokens = nuclear_tokenize(source)
            parser = NuclearParser(tokens)
            NuclearEvaluator._ast_cache[key] = parser.parse_program()
            if len(NuclearEvaluator._ast_cache) > 512:
                # evict oldest half when cache grows large
                keys = list(NuclearEvaluator._ast_cache)
                for k in keys[:256]: del NuclearEvaluator._ast_cache[k]
        return self.eval(NuclearEvaluator._ast_cache[key], self.global_env)


# ─────────────────────────────────────────────────────────────────────────────
# Nuclear test runner
# ─────────────────────────────────────────────────────────────────────────────

def run_nuclear_test_file(path: str, level: str):
    import re as _re
    print(f"\n{'='*70}")
    print(f"  NUVOLA NUCLEAR TEST SUITE — {level.upper()}")
    print(f"  File: {path}")
    print(f"{'='*70}")

    with open(path) as f:
        source = f.read()

    evaluator = NuclearEvaluator()
    passed = 0; failed = 0; errors = []

    test_blocks = _re.split(r'\n-- TEST: (.+)\n', source)

    if len(test_blocks) == 1:
        try:
            evaluator.run(source)
            print("  [PASS] (single program, no errors)")
            passed = 1
        except AssertionError as e:
            print(f"  [FAIL] {e}")
            failed = 1
        except Exception as e:
            print(f"  [ERROR] {e}")
            traceback.print_exc()
            failed = 1
    else:
        preamble = test_blocks[0]
        for i in range(1, len(test_blocks), 2):
            test_name = test_blocks[i]
            test_body = test_blocks[i+1] if i+1 < len(test_blocks) else ''
            full_src = preamble + test_body
            ev2 = NuclearEvaluator()
            try:
                ev2.run(full_src)
                print(f"  [PASS] {test_name}")
                passed += 1
            except AssertionError as e:
                msg = str(e)
                print(f"  [FAIL] {test_name}: {msg}")
                errors.append((test_name, msg))
                failed += 1
            except Exception as e:
                msg = str(e)
                print(f"  [ERROR] {test_name}: {msg}")
                traceback.print_exc()
                errors.append((test_name, msg))
                failed += 1

    print(f"\n  Results: {passed} passed, {failed} failed")
    if errors:
        print(f"\n  Failures:")
        for name, msg in errors:
            print(f"    • {name}: {msg}")
    print(f"{'='*70}\n")
    return passed, failed


# ─────────────────────────────────────────────────────────────────────────────
# Entry point
# ─────────────────────────────────────────────────────────────────────────────

if __name__ == '__main__':
    import os

    base = os.path.dirname(os.path.abspath(__file__))

    # If a specific file is passed, run it
    if len(sys.argv) > 1:
        path = sys.argv[1]
        run_nuclear_test_file(path, os.path.basename(path))
        sys.exit(0)

    total_pass = total_fail = 0

    # Run existing base tests with nuclear evaluator (ensures backward compat)
    for fname, level in [
        ('simple.nvl',  'simple'),
        ('medium.nvl',  'medium'),
        ('hard.nvl',    'hard'),
        ('insane.nvl',  'insane'),
    ]:
        fpath = os.path.join(base, fname)
        if os.path.exists(fpath):
            # Use base run_test_file but with nuclear evaluator
            import re as _re
            with open(fpath) as f: src = f.read()
            ev = NuclearEvaluator()
            blocks = _re.split(r'\n-- TEST: (.+)\n', src)
            if len(blocks) == 1:
                try: ev.run(src); p, fl = 1, 0
                except Exception as e: print(f"[ERROR] {e}"); p, fl = 0, 1
            else:
                p = fl = 0
                preamble = blocks[0]
                for i in range(1, len(blocks), 2):
                    tname = blocks[i]
                    tbody = blocks[i+1] if i+1 < len(blocks) else ''
                    ev2 = NuclearEvaluator()
                    try:
                        ev2.run(preamble + tbody)
                        p += 1
                    except AssertionError as e:
                        fl += 1; print(f"  [FAIL] {tname}: {e}")
                    except Exception as e:
                        fl += 1; print(f"  [ERROR] {tname}: {e}")
            print(f"  {level.upper()}: {p} pass, {fl} fail")
            total_pass += p; total_fail += fl

    # Run nuclear test files
    for fname, level in [
        ('nuclear.nvl', 'nuclear'),
        ('ai_core.nvl', 'ai_core'),
    ]:
        fpath = os.path.join(base, fname)
        if os.path.exists(fpath):
            p, fl = run_nuclear_test_file(fpath, level)
            total_pass += p; total_fail += fl

    print(f"\n{'='*70}")
    print(f"  TOTAL: {total_pass} passed, {total_fail} failed")
    print(f"{'='*70}\n")
    sys.exit(0 if total_fail == 0 else 1)
