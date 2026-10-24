// VST2 のプラグインの形（関数の並びと構造体）だけを自分で書いたもの。SDK は使わない（試すための DLL を作るため）
#pragma once
#include <stdint.h>

#define VST_MAGIC 0x56737450 /* 'VstP' */

typedef struct AEffect AEffect;
typedef intptr_t (*audioMasterCallback)(AEffect *effect, int32_t opcode, int32_t index, intptr_t value, void *ptr, float opt);
typedef intptr_t (*AEffectDispatcherProc)(AEffect *effect, int32_t opcode, int32_t index, intptr_t value, void *ptr, float opt);
typedef void (*AEffectProcessProc)(AEffect *effect, float **inputs, float **outputs, int32_t sampleFrames);
typedef void (*AEffectSetParameterProc)(AEffect *effect, int32_t index, float parameter);
typedef float (*AEffectGetParameterProc)(AEffect *effect, int32_t index);

struct AEffect {
  int32_t magic;
  AEffectDispatcherProc dispatcher;
  AEffectProcessProc process;
  AEffectSetParameterProc setParameter;
  AEffectGetParameterProc getParameter;
  int32_t numPrograms;
  int32_t numParams;
  int32_t numInputs;
  int32_t numOutputs;
  int32_t flags;
  intptr_t resvd1;
  intptr_t resvd2;
  int32_t initialDelay;
  int32_t realQualities;
  int32_t offQualities;
  float ioRatio;
  void *object;
  void *user;
  int32_t uniqueID;
  int32_t version;
  AEffectProcessProc processReplacing;
  AEffectProcessProc processDoubleReplacing;
  char future[56];
};

/* dispatcher の opcode（使うものだけ） */
enum {
  effOpen = 0,
  effClose = 1,
  effGetParamName = 8,
  effSetSampleRate = 10,
  effSetBlockSize = 11,
  effMainsChanged = 12,
  effProcessEvents = 25,
  effGetEffectName = 45,
};

/* flags */
enum { effFlagsCanReplacing = 1 << 4, effFlagsIsSynth = 1 << 8 };

/* effProcessEvents で受け取る MIDI（使うものだけ） */
enum { kVstMidiType = 1 };
typedef struct {
  int32_t type;
  int32_t byteSize;
  int32_t deltaFrames;
  int32_t flags;
  int32_t noteLength;
  int32_t noteOffset;
  char midiData[4];
  char detune;
  char noteOffVelocity;
  char reserved1;
  char reserved2;
} VstMidiEvent;
typedef struct {
  int32_t numEvents;
  intptr_t reserved;
  VstMidiEvent *events[2];
} VstEvents;
