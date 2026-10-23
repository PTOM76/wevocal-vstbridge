// JIT を確かめるための VST2。音の処理で SSE の型の変換（cvt）、並べ替え（shufps、unpcklps）、movd、movq を使う。
// 範囲外の変換（cvtt の 0x80000000）も通る。外部の DLL を import しない（-nostdlib）
#include "vst2.h"
#include <emmintrin.h>

static float amount = 0.5f;
static double state[2];
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
    copyName((char *)ptr, "WeVocal Test SSE");
    return 1;
  case effGetParamName:
    copyName((char *)ptr, "Amount");
    return 1;
  default:
    return 0;
  }
}

/// 1 サンプルを、いろいろな変換を通して作り直す
static float shape(float x, int c) {
  // float → int（切り捨て）→ float: 量子化（cvttss2si、cvtsi2ss）
  int q = _mm_cvttss_si32(_mm_set_ss(x * 4096.0f));
  float y = _mm_cvtss_f32(_mm_cvtsi32_ss(_mm_setzero_ps(), q)) / 4096.0f;
  // 範囲外の変換は 0x80000000（cvttss2si）
  int big = _mm_cvttss_si32(_mm_set_ss(x * 1e10f));
  // double の状態で 1 次のフィルター（cvtss2sd、cvtsd2ss、cvttsd2si、cvtsi2sd）
  __m128d d = _mm_cvtss_sd(_mm_setzero_pd(), _mm_set_ss(y));
  state[c] = state[c] * 0.9 + _mm_cvtsd_f64(d) * 0.1 * amount;
  long long w = _mm_cvttsd_si64(_mm_set_sd(state[c] * 65536.0));
  double back = _mm_cvtsd_f64(_mm_cvtsi64_sd(_mm_setzero_pd(), w)) / 65536.0;
  float z = _mm_cvtss_f32(_mm_cvtsd_ss(_mm_setzero_ps(), _mm_set_sd(back)));
  // 並べ替え（shufps、unpcklps）と、整数のレジスタとの行き来（movd、movq）
  __m128 v = _mm_set_ps(z, y, x, (float)(big < 0));
  __m128 s = _mm_shuffle_ps(v, v, _MM_SHUFFLE(0, 3, 1, 2));
  __m128 u = _mm_unpacklo_ps(s, v);
  int bits = _mm_cvtsi128_si32(_mm_castps_si128(u));
  __m128i r = _mm_cvtsi32_si128(bits);
  long long b64 = _mm_cvtsi128_si64(_mm_castps_si128(_mm_movehl_ps(u, u)));
  __m128i r2 = _mm_cvtsi64_si128(b64);
  // 範囲外の変換の結果（0x80000000 か、飽和した値か）が音に出るようにする
  return _mm_cvtss_f32(_mm_castsi128_ps(r)) * 0.5f + _mm_cvtss_f32(_mm_castsi128_ps(r2)) * 0.25f + z + (float)big * 1e-12f;
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
  effect.uniqueID = 0x57565353; /* 'WVSS' */
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
