/* Sleep diagnostic for the NOTE Emulator (fixtures/diag-fw/sleep), PATCHES #18.
 * 1. Light sleep with a 500 ms timer wake: "DIAG light timer cause=<n> slept_ms=<esp_timer delta>".
 * 2. Light sleep with UP (GPIO39) low as the wake source: "DIAG light gpio cause=<n> at_ms=<t>";
 *    the host presses UP some time after "DIAG waiting for UP".
 * 3. Deep sleep for 1 s, twice: "DIAG boot n=<RTC counter> reset=<reason> cause=<n>", then done. */
#include <stdio.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "driver/gpio.h"
#include "esp_sleep.h"
#include "esp_timer.h"
#include "esp_system.h"

static RTC_DATA_ATTR int boots;

void app_main(void)
{
    gpio_config_t latch = {.pin_bit_mask = 1ULL << 17, .mode = GPIO_MODE_OUTPUT};
    gpio_config(&latch);
    gpio_set_level(17, 1);
    boots++;
    printf("DIAG boot n=%d reset=%d cause=%d\n", boots, (int)esp_reset_reason(), (int)esp_sleep_get_wakeup_cause());
    if (boots == 1) {
        esp_sleep_enable_timer_wakeup(500 * 1000);
        int64_t t0 = esp_timer_get_time();
        esp_light_sleep_start();
        printf("DIAG light timer cause=%d slept_ms=%lld\n", (int)esp_sleep_get_wakeup_cause(), (esp_timer_get_time() - t0) / 1000);
        esp_sleep_disable_wakeup_source(ESP_SLEEP_WAKEUP_TIMER);

        gpio_config_t up = {.pin_bit_mask = 1ULL << 39, .mode = GPIO_MODE_INPUT, .pull_up_en = GPIO_PULLUP_ENABLE};
        gpio_config(&up);
        gpio_wakeup_enable(39, GPIO_INTR_LOW_LEVEL);
        esp_sleep_enable_gpio_wakeup();
        printf("DIAG waiting for UP\n");
        fflush(stdout);
        vTaskDelay(pdMS_TO_TICKS(50));
        esp_light_sleep_start();
        printf("DIAG light gpio cause=%d at_ms=%lld\n", (int)esp_sleep_get_wakeup_cause(), esp_timer_get_time() / 1000);
        gpio_wakeup_disable(39);
    }
    if (boots <= 2) {
        fflush(stdout);
        esp_deep_sleep(1000 * 1000);
    }
    printf("DIAG done\n");
}
