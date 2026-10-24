// MIDI で鳴る一番小さな楽器の VST2（入力 0、出力 2）。effProcessEvents のノートで、音の高さの違う 8 つの声を鳴らす。外部の DLL を import しない（-nostdlib）
#include "vst2.h"

#define VOICES 8

static float level = 0.5f;
static float rate = 48000.0f;
/// 声ごとの位相（0〜1）、1 サンプルの進み、音量、鳴らしているノート（-1 は空き）
static float phase[VOICES], step[VOICES], amp[VOICES];
static int note[VOICES] = {-1, -1, -1, -1, -1, -1, -1, -1};

static void copyName(char *dst, const char *src) {
  while ((*dst++ = *src++)) {
  }
}

/// ノートの周波数（A4 = 69 = 440 Hz。libm を使わず、半音の比を掛けて求める）
static float noteHz(int n) {
  float f = 440.0f;
  for (; n > 69; n--) f *= 1.0594631f;
  for (; n < 69; n++) f /= 1.0594631f;
  return f;
}

static void midi(const char *d) {
  int status = d[0] & 0xf0, n = d[1] & 0x7f, vel = d[2] & 0x7f;
  if (status == 0x90 && vel > 0) {
    for (int v = 0; v < VOICES; v++)
      if (note[v] < 0) {
        note[v] = n;
        phase[v] = 0.0f;
        step[v] = noteHz(n) / rate;
        amp[v] = (float)vel / 127.0f;
        return;
      }
  } else if (status == 0x80 || status == 0x90) {
    for (int v = 0; v < VOICES; v++)
      if (note[v] == n) note[v] = -1;
  }
}

static intptr_t dispatcher(AEffect *e, int32_t opcode, int32_t index, intptr_t value, void *ptr, float opt) {
  (void)e;
  (void)index;
  (void)value;
  switch (opcode) {
  case effGetEffectName:
    copyName((char *)ptr, "WeVocal Test Synth");
    return 1;
  case effGetParamName:
    copyName((char *)ptr, "Level");
    return 1;
  case effSetSampleRate:
    rate = opt;
    return 0;
  case effProcessEvents: {
    VstEvents *ev = (VstEvents *)ptr;
    for (int32_t k = 0; k < ev->numEvents; k++)
      if (ev->events[k]->type == kVstMidiType) midi(ev->events[k]->midiData);
    return 1;
  }
  default:
    return 0;
  }
}

/// 位相（0〜1）の正弦の近似（放物線を 2 つつなぐ）
static float sine(float p) {
  float x = p * 2.0f - 1.0f;
  float y = 4.0f * x * (1.0f - (x < 0.0f ? -x : x));
  return -y;
}

static void processReplacing(AEffect *e, float **in, float **out, int32_t n) {
  (void)e;
  (void)in;
  for (int32_t i = 0; i < n; i++) {
    float l = 0.0f, r = 0.0f;
    for (int v = 0; v < VOICES; v++) {
      if (note[v] < 0) continue;
      float s = sine(phase[v]) * amp[v];
      phase[v] += step[v];
      if (phase[v] >= 1.0f) phase[v] -= 1.0f;
      // 声ごとに左右へ振り分ける
      l += s * (float)(VOICES - v) / VOICES;
      r += s * (float)(v + 1) / VOICES;
    }
    out[0][i] = l * level;
    out[1][i] = r * level;
  }
}

static void setParameter(AEffect *e, int32_t index, float v) {
  (void)e;
  if (index == 0) level = v;
}

static float getParameter(AEffect *e, int32_t index) {
  (void)e;
  return index == 0 ? level : 0.0f;
}

static AEffect effect;

__declspec(dllexport) AEffect *VSTPluginMain(audioMasterCallback cb) {
  (void)cb;
  effect.magic = VST_MAGIC;
  effect.dispatcher = dispatcher;
  effect.process = processReplacing;
  effect.processReplacing = processReplacing;
  effect.setParameter = setParameter;
  effect.getParameter = getParameter;
  effect.numPrograms = 1;
  effect.numParams = 1;
  effect.numInputs = 0;
  effect.numOutputs = 2;
  effect.flags = effFlagsCanReplacing | effFlagsIsSynth;
  effect.uniqueID = 0x57565359; /* 'WVSY' */
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
