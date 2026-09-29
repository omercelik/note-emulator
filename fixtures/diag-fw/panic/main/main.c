/* Panic diagnostic for the NOTE Emulator (fixtures/diag-fw/panic), DEV-03/DEV-04.
 * diag_panic_here() stores through a NULL pointer on the line marked FAULT; the local
 * `local_marker` holds 0x5a5a at that point. The panic handler prints a backtrace, writes an
 * ELF core dump to the `coredump` partition, then halts. */
#include <stdio.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

volatile int *volatile diag_target = NULL;

void __attribute__((noinline)) diag_panic_here(int marker)
{
    volatile int local_marker = marker;
    printf("DIAG panic about to fault, marker=%d\n", local_marker);
    *diag_target = local_marker; /* FAULT */
}

static void diag_crash_task(void *arg)
{
    vTaskDelay(pdMS_TO_TICKS(50));
    diag_panic_here(0x5a5a);
}

void app_main(void)
{
    xTaskCreate(diag_crash_task, "diag_crash", 4096, NULL, 5, NULL);
}
