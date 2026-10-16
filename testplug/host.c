// ネイティブの Windows で VST2 の DLL を動かす小さなホスト（エミュレーターの結果と比べる基準）。
// 使い方: host.exe gain.dll → 1 kHz の正弦波 8 サンプルを通した結果と、名前、つまみの値を表示する
#include <math.h>
#include <stdio.h>
#include <windows.h>
#include "vst2.h"

static intptr_t hostCallback(AEffect *e, int32_t opcode, int32_t index, intptr_t value, void *ptr, float opt) {
  (void)e;
  (void)index;
  (void)value;
  (void)ptr;
  (void)opt;
  return opcode == 1 ? 2400 : 0; /* audioMasterVersion */
}

int main(int argc, char **argv) {
  HMODULE m = LoadLibraryA(argc > 1 ? argv[1] : "gain.dll");
  if (!m) return printf("load failed\n"), 1;
  AEffect *(*entry)(audioMasterCallback) = (void *)GetProcAddress(m, "VSTPluginMain");
  AEffect *e = entry(hostCallback);
  char name[64] = {0};
  e->dispatcher(e, effGetEffectName, 0, 0, name, 0);
  printf("magic=%08x version=%d params=%d name=%s\n", e->magic, e->version, e->numParams, name);
  e->setParameter(e, 0, 0.25f);
  printf("param0=%g\n", e->getParameter(e, 0));
  float l[8], r[8], ol[8], or_[8];
  for (int i = 0; i < 8; i++) l[i] = r[i] = sinf(2 * 3.14159265f * 1000 * i / 48000);
  float *in[2] = {l, r}, *out[2] = {ol, or_};
  e->processReplacing(e, in, out, 8);
  for (int i = 0; i < 8; i++) printf("%.6f ", ol[i]);
  printf("\n");
  return 0;
}
