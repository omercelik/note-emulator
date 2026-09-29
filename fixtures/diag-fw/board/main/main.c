/* Board diagnostic for the NOTE Emulator (fixtures/diag-fw/board), both NOTE devices.
 * BATT-02: every 250 ms, "DIAG batt mv=N chg=L full=L" (ADC1_CH3 through the 1:2 divider with
 * eFuse calibration, charger pins GPIO2/GPIO1 as raw levels) whenever a value changes.
 * RTC-01: sets the PCF8563 to 2026-12-31 23:59:58, reads it back across the year rollover,
 * arms a minute alarm for 00:01 and reports the INT (GPIO5, active low) interrupt. */
#include <stdio.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "esp_adc/adc_oneshot.h"
#include "esp_adc/adc_cali.h"
#include "esp_adc/adc_cali_scheme.h"

#define PIN_LATCH 17
#define PIN_CHG 2
#define PIN_FULL 1
#define PIN_RTC_INT 5

static i2c_master_dev_handle_t rtc;
static volatile int alarms;

static uint8_t bcd(int v) { return (uint8_t)(((v / 10) << 4) | (v % 10)); }
static int dec(uint8_t b) { return (b >> 4) * 10 + (b & 15); }

static void rtc_write(uint8_t reg, const uint8_t *v, int n)
{
    uint8_t buf[17] = {reg};
    for (int i = 0; i < n; i++) buf[1 + i] = v[i];
    ESP_ERROR_CHECK(i2c_master_transmit(rtc, buf, n + 1, 100));
}

static void rtc_read(uint8_t reg, uint8_t *v, int n)
{
    ESP_ERROR_CHECK(i2c_master_transmit_receive(rtc, &reg, 1, v, n, 100));
}

static void print_time(const char *what)
{
    uint8_t t[7];
    rtc_read(0x02, t, 7);
    printf("DIAG rtc %s 20%02d-%02d-%02d %02d:%02d:%02d vl=%d\n", what, dec(t[6]), dec(t[5] & 0x1f), dec(t[3] & 0x3f),
           dec(t[2] & 0x3f), dec(t[1] & 0x7f), dec(t[0] & 0x7f), t[0] >> 7);
}

static void IRAM_ATTR on_rtc_int(void *arg) { alarms++; }

static void rtc_test(void)
{
    i2c_master_bus_config_t bus = {.i2c_port = 0, .sda_io_num = 47, .scl_io_num = 48, .clk_source = I2C_CLK_SRC_DEFAULT,
                                   .glitch_ignore_cnt = 7, .flags.enable_internal_pullup = true};
    i2c_master_bus_handle_t h;
    ESP_ERROR_CHECK(i2c_new_master_bus(&bus, &h));
    i2c_device_config_t dev = {.dev_addr_length = I2C_ADDR_BIT_LEN_7, .device_address = 0x51, .scl_speed_hz = 100000};
    ESP_ERROR_CHECK(i2c_master_bus_add_device(h, &dev, &rtc));
    const uint8_t ctl[2] = {0x00, 0x00};
    rtc_write(0x00, ctl, 2);
    /* 2026-12-31 (Thursday) 23:59:58 */
    const uint8_t t[7] = {bcd(58), bcd(59), bcd(23), bcd(31), 4, bcd(12), bcd(26)};
    rtc_write(0x02, t, 7);
    print_time("set");
    vTaskDelay(pdMS_TO_TICKS(3000));
    print_time("later");
    /* Alarm at minute 01, hour/day/weekday disabled (AE = bit 7 set). */
    const uint8_t alarm[4] = {bcd(1), 0x80, 0x80, 0x80};
    rtc_write(0x09, alarm, 4);
    gpio_config_t in = {.pin_bit_mask = 1ULL << PIN_RTC_INT, .mode = GPIO_MODE_INPUT, .pull_up_en = GPIO_PULLUP_ENABLE,
                        .intr_type = GPIO_INTR_NEGEDGE};
    gpio_config(&in);
    gpio_isr_handler_add(PIN_RTC_INT, on_rtc_int, NULL);
    const uint8_t aie = 0x02;
    rtc_write(0x01, &aie, 1);
    printf("DIAG rtc armed\n");
}

static void rtc_poll(void)
{
    static int seen;
    if (alarms == seen) return;
    seen = alarms;
    uint8_t c2;
    rtc_read(0x01, &c2, 1);
    print_time("alarm");
    printf("DIAG rtc af=%d int=%d\n", (c2 >> 3) & 1, gpio_get_level(PIN_RTC_INT));
    const uint8_t clear = 0x02; /* keep AIE, clear AF */
    rtc_write(0x01, &clear, 1);
    printf("DIAG rtc cleared int=%d\n", gpio_get_level(PIN_RTC_INT));
}

void app_main(void)
{
    gpio_config_t latch = {.pin_bit_mask = 1ULL << PIN_LATCH, .mode = GPIO_MODE_OUTPUT};
    gpio_config(&latch);
    gpio_set_level(PIN_LATCH, 1);
    gpio_config_t chg = {.pin_bit_mask = (1ULL << PIN_CHG) | (1ULL << PIN_FULL), .mode = GPIO_MODE_INPUT};
    gpio_config(&chg);
    gpio_install_isr_service(0);

    adc_oneshot_unit_handle_t adc;
    adc_oneshot_unit_init_cfg_t unit = {.unit_id = ADC_UNIT_1};
    ESP_ERROR_CHECK(adc_oneshot_new_unit(&unit, &adc));
    adc_oneshot_chan_cfg_t ch = {.atten = ADC_ATTEN_DB_12, .bitwidth = ADC_BITWIDTH_12};
    ESP_ERROR_CHECK(adc_oneshot_config_channel(adc, ADC_CHANNEL_3, &ch));
    adc_cali_handle_t cali;
    adc_cali_curve_fitting_config_t cc = {.unit_id = ADC_UNIT_1, .chan = ADC_CHANNEL_3, .atten = ADC_ATTEN_DB_12, .bitwidth = ADC_BITWIDTH_12};
    ESP_ERROR_CHECK(adc_cali_create_scheme_curve_fitting(&cc, &cali));

    rtc_test();
    int last_mv = -1, last_chg = -1, last_full = -1;
    for (;;) {
        int sum = 0, raw, mv;
        for (int i = 0; i < 8; i++) {
            adc_oneshot_read(adc, ADC_CHANNEL_3, &raw);
            sum += raw;
        }
        adc_cali_raw_to_voltage(cali, sum / 8, &mv);
        mv *= 2;
        int c = gpio_get_level(PIN_CHG), f = gpio_get_level(PIN_FULL);
        /* 30 mV hysteresis keeps ADC noise out of the log. */
        if (last_mv < 0 || mv > last_mv + 30 || mv < last_mv - 30 || c != last_chg || f != last_full) {
            last_mv = mv; last_chg = c; last_full = f;
            printf("DIAG batt mv=%d chg=%d full=%d\n", mv, c, f);
        }
        rtc_poll();
        vTaskDelay(pdMS_TO_TICKS(250));
    }
}
