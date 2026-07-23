#include <CoreAudio/AudioServerPlugIn.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

extern void* NullAudio_Create(CFAllocatorRef allocator, CFUUIDRef requestedType);

enum
{
    kStaticStreamBox = 2,
    kStaticStreamInput = 4,
    kStaticStreamOutput = 8,
    kFrames = 128
};

static int checkPropertyErrorReleasesLock(AudioServerPlugInDriverRef driver)
{
    AudioObjectPropertyAddress address = {
        kAudioBoxPropertyDeviceList,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain
    };
    AudioObjectID device = 0;
    UInt32 outputSize = 0;
    OSStatus status = (*driver)->GetPropertyData(
        driver,
        kStaticStreamBox,
        1,
        &address,
        0,
        NULL,
        0,
        &outputSize,
        &device);
    if(status != kAudioHardwareBadPropertySizeError)
    {
        fprintf(stderr, "Short box device-list query returned %d\n", status);
        return 1;
    }
    return 0;
}

static int checkLoopback(AudioServerPlugInDriverRef driver)
{
    AudioServerPlugInIOCycleInfo cycle = {0};
    cycle.mOutputTime.mSampleTime = 2048;
    cycle.mInputTime.mSampleTime = 2048 + 512;

    Float32 output[kFrames * 2];
    Float32 input[kFrames * 2];
    for(UInt32 frame = 0; frame < kFrames; ++frame)
    {
        output[frame * 2] = (Float32)frame / kFrames;
        output[(frame * 2) + 1] = -output[frame * 2];
    }
    memset(input, 0, sizeof(input));

    OSStatus status = (*driver)->DoIOOperation(
        driver,
        3,
        kStaticStreamOutput,
        1,
        kAudioServerPlugInIOOperationWriteMix,
        kFrames,
        &cycle,
        output,
        NULL);
    if(status != noErr)
    {
        fprintf(stderr, "WriteMix failed: %d\n", status);
        return 1;
    }

    status = (*driver)->DoIOOperation(
        driver,
        3,
        kStaticStreamInput,
        2,
        kAudioServerPlugInIOOperationReadInput,
        kFrames,
        &cycle,
        input,
        NULL);
    if(status != noErr)
    {
        fprintf(stderr, "ReadInput failed: %d\n", status);
        return 1;
    }

    for(UInt32 sample = 0; sample < kFrames * 2; ++sample)
    {
        if(fabsf(input[sample] - output[sample]) > 0.000001f)
        {
            fprintf(
                stderr,
                "Loopback mismatch at sample %u: expected %f, got %f\n",
                sample,
                output[sample],
                input[sample]);
            return 1;
        }
    }
    return 0;
}

int main(void)
{
    AudioServerPlugInDriverRef driver = NullAudio_Create(
        kCFAllocatorDefault,
        kAudioServerPlugInTypeUUID);
    if(driver == NULL)
    {
        fputs("Factory did not create an AudioServerPlugIn driver.\n", stderr);
        return 1;
    }

    int result = checkPropertyErrorReleasesLock(driver);
    if(result != 0)
    {
        return result;
    }

    OSStatus status = (*driver)->StartIO(driver, 3, 1);
    if(status != noErr)
    {
        fprintf(stderr, "StartIO failed: %d\n", status);
        return 1;
    }
    result = checkLoopback(driver);
    status = (*driver)->StopIO(driver, 3, 1);
    if(status != noErr)
    {
        fprintf(stderr, "StopIO failed: %d\n", status);
        return 1;
    }
    if(result == 0)
    {
        puts("Static Stream audio loopback test passed.");
    }
    return result;
}
