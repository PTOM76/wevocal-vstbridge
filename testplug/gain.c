// 試すための一番小さな VST2（音量を変えるだけ）。外部の DLL を import しない（-nostdlib）ので、エミュレーターの最初の確かめに使える
#include "vst2.h"

static float gain = 0.5f;
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
    copyName((char *)ptr, "WeVocal Test Gain");
    return 1;
  case effGetParamName:
    copyName((char *)ptr, "Gain");
    return 1;
  default:
    return 0;
  }
}

static void processReplacing(AEffect *e, float **in, float **out, int32_t n) {
  (void)e;
  for (int32_t c = 0; c < 2; c++)
    for (int32_t i = 0; i < n; i++) out[c][i] = in[c][i] * gain;
}

static void setParameter(AEffect *e, int32_t index, float v) {
  (void)e;
  if (index == 0) gain = v;
}

static float getParameter(AEffect *e, int32_t index) {
  (void)e;
  return index == 0 ? gain : 0.0f;
}

static AEffect effect;

__declspec(dllexport) AEffect *VSTPluginMain(audioMasterCallback cb) {
  host = cb;
  // 版をホストに尋ねる（ホストの呼び出しの確かめ。audioMasterVersion = 1）
  int32_t hostVersion = (int32_t)host(0, 1, 0, 0, 0, 0.0f);
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
  effect.uniqueID = 0x57564754; /* 'WVGT' */
  effect.version = hostVersion;
  return &effect;
}

// DLL の入口（何もしない。-nostdlib なので自分で用意する）
int __stdcall DllMainCRTStartup(void *h, unsigned long reason, void *r) {
  (void)h;
  (void)reason;
  (void)r;
  return 1;
}
