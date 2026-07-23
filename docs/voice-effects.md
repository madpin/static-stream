# Voice Effects

Static Stream processes voice effects locally and sends the result directly to **Static
Microphone**. No model, network connection, plug-in, or third-party audio device is required.
Sound clips are mixed after voice processing and retain their original sound.

![Static Stream voice-effect controls](images/voice-effects.png)

## Presets

| Preset | Processing |
| --- | --- |
| **Clean** | Sample-exact bypass |
| **Deep** | Downward pitch shift, low-pass filtering, and gentle saturation |
| **Robot** | Ring modulation |
| **Anonymous** | Downward pitch shift, band limiting, modulation, and saturation |
| **Radio** | Narrow-band filtering and saturation |
| **Alien** | Upward pitch shift and modulation |
| **Tiny** | Strong upward pitch shift and high-pass filtering |
| **Demon** | Strong downward pitch shift, dark filtering, modulation, and saturation |

**Intensity** controls how strongly the preset changes the source. **Effect mix** blends processed
voice with the original physical microphone. Both parameters change smoothly to avoid clicks.

The **Anonymous** preset is an entertainment effect, not an identity-protection system. Speaking
style, cadence, accent, room sound, and other characteristics can remain identifiable.

## Operation

1. Select **Static Microphone** in the meeting or recording app.
2. In Static Stream, select a physical microphone and a voice-effect preset.
3. Speak and confirm that **Physical microphone**, **Processed voice**, and **Virtual microphone**
   show activity.
4. Adjust **Intensity** and **Effect mix**.

Press `Option+Command+V` to select the next preset from anywhere in macOS. Direct preset choices are
also available under **Voice effect** in the Static Stream menu-bar menu. **Clean** bypasses the
processor without changing the saved Intensity or Effect mix values.

## Signal And Latency

Pitch presets use a dual-grain time-domain shifter with overlapping windows. Its storage is
allocated when the audio engine starts, and its practical delay varies between roughly 2.5 ms and
35 ms. Modulation and filter-only presets add no block delay. The configured device-routing
latency remains independent of this processing delay.

Preset changes crossfade over 25 ms. Intensity and mix values ramp over 20 ms. These transitions
run within fixed preallocated state and do not lock, allocate, log, or access disk from the audio
callback.

## Troubleshooting

**The meters move but the meeting app sounds clean**

- Confirm the meeting app uses **Static Microphone**, not the physical microphone.
- Confirm the selected preset is not **Clean** and both controls are above 0%.
- Check Activity for both the requested preset and the audio-engine activation event.

**A pitch preset starts quietly**

The pitch shifter needs up to 35 ms of recent microphone audio when first activated. Static Stream
crossfades during that warm-up to prevent a click.

**The voice is distorted**

Reduce Intensity, Effect mix, or the physical microphone input level. Saturation is intentional in
Radio, Anonymous, and Demon, but an already-clipping physical input cannot be repaired downstream.
