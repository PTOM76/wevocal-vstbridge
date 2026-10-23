// JIT を確かめるための VST2。音の処理で、引数が 1 つの mul と imul、32 と 64 ビットの div と idiv、switch の表（間接の jmp）、
// x87 の fsin、fcos、fpatan を使う。外部の DLL を import しない（-nostdlib）
#include "vst2.h"
#include <stdint.h>

static float amount = 0.5f;
static uint32_t seed[2] = {1, 7};
static audioMasterCallback host;

static void copyName(char *dst, const char *src) {
  while ((*dst++ = *src++)) {
  }
}

static intptr_t dispatcher(AEffect *e, int32_t opcode, int32_t index, intptr_t value, void *ptr, float opt) {
  (void)e;
  (void)index;
  (void)value;
  (void)opt;
  switch (opcode) {
  case effGetEffectName:
    copyName((char *)ptr, "WeVocal Test IntOps");
    return 1;
  case effGetParamName:
    copyName((char *)ptr, "Amount");
    return 1;
  default:
    return 0;
  }
}

/// 引数が 1 つの mul（edx:eax = eax × r）。上位と下位を混ぜて返す
static uint32_t mul1(uint32_t a, uint32_t b) {
  uint32_t lo, hi;
  __asm__("mull %3" : "=a"(lo), "=d"(hi) : "a"(a), "r"(b) : "cc");
  return lo ^ hi;
}
/// 引数が 1 つの imul
static int32_t imul1(int32_t a, int32_t b) {
  int32_t lo, hi;
  __asm__("imull %3" : "=a"(lo), "=d"(hi) : "a"(a), "r"(b) : "cc");
  return lo + hi;
}
/// 64 ビットの div（rdx:rax ÷ r）。rdx が 0 でないときも通す
static uint64_t div64(uint64_t hi, uint64_t lo, uint64_t d) {
  uint64_t q, r;
  __asm__("divq %4" : "=a"(q), "=d"(r) : "a"(lo), "d"(hi), "r"(d) : "cc");
  return q ^ r;
}
/// 64 ビットの idiv（cqo のあと）
static int64_t idiv64(int64_t a, int64_t d) {
  int64_t q, r;
  __asm__("cqo; idivq %3" : "=a"(q), "=&d"(r) : "a"(a), "r"(d) : "cc");
  return q * 3 + r;
}
/// x87 の fsin、fcos、fpatan
static double trig(double x) {
  double s, c, t;
  __asm__("fsin" : "=t"(s) : "0"(x));
  __asm__("fcos" : "=t"(c) : "0"(x));
  __asm__("fpatan" : "=t"(t) : "0"(x), "u"(1.0) : "st(1)");
  return s + c * 0.5 + t * 0.25;
}

/// 1 サンプル。整数の擬似乱数を、いろいろな整数の命令で混ぜる
static float shape(float x, int c) {
  uint32_t s = seed[c] = seed[c] * 1664525u + 1013904223u;
  int32_t v = (int32_t)(x * 32768.0f);
  uint32_t m = mul1(s, (uint32_t)v | 1u);
  int32_t im = imul1(v, (int32_t)s >> 8);
  uint64_t dv = div64(s & 3, (uint64_t)m << 20 | s, (s >> 4) | 0x100000000ull);
  int64_t iv = idiv64((int64_t)v * 100003 - (int64_t)s, (int64_t)(s % 97) - 48 | 1);
  int32_t sw;
  switch (s % 7) {
  case 0: sw = v / 3; break;
  case 1: sw = v % 5; break;
  case 2: sw = (int32_t)(m >> 16); break;
  case 3: sw = im; break;
  case 4: sw = (int32_t)dv; break;
  case 5: sw = (int32_t)iv; break;
  default: sw = -v; break;
  }
  double t = trig((double)x * 3.0 + (double)(s & 255) / 64.0);
  return x * amount + (float)sw * 1e-9f + (float)t * 0.01f;
}

static void processReplacing(AEffect *e, float **in, float **out, int32_t n) {
  (void)e;
  for (int32_t c = 0; c < 2; c++)
    for (int32_t i = 0; i < n; i++) out[c][i] = shape(in[c][i], c);
}

static void setParameter(AEffect *e, int32_t index, float v) {
  (void)e;
  if (index == 0) amount = v;
}

static float getParameter(AEffect *e, int32_t index) {
  (void)e;
  return index == 0 ? amount : 0.0f;
}

static AEffect effect;

__declspec(dllexport) AEffect *VSTPluginMain(audioMasterCallback cb) {
  host = cb;
  effect.magic = VST_MAGIC;
  effect.dispatcher = dispatcher;
  effect.process = processReplacing;
  effect.processReplacing = processReplacing;
  effect.setParameter = setParameter;
  effect.getParameter = getParameter;
  effect.numPrograms = 1;
  effect.numParams = 1;
  effect.numInputs = 2;
  effect.numOutputs = 2;
  effect.flags = effFlagsCanReplacing;
  effect.uniqueID = 0x5756494f; /* 'WVIO' */
  effect.version = 1;
  return &effect;
}

// DLL の入口（何もしない。-nostdlib なので自分で用意する）
int __stdcall DllMainCRTStartup(void *h, unsigned long reason, void *r) {
  (void)h;
  (void)reason;
  (void)r;
  return 1;
}
