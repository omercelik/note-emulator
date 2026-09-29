/* SSD2683 edge-path diagnostic for the NOTE Emulator (fixtures/diag-fw/panel), NOTE4 wiring.
 * DISP-03: each step prints "DIAG step <name> busy_us=N" (BUSY low time after the last
 * command, measured by the guest) and then holds for 500 ms so the host can inspect the panel:
 *   full      stripes (even rows white, odd rows black), OTP full refresh
 *   short     only the first 1000 RAM bytes (10 rows) black, refresh
 *   unknown   a 535-byte external LUT that is no known pass, refresh; then OTP mode again
 *   reset     all white, refresh, EN-style RST pulse 20 ms into the refresh
 *   unpowered rail off, 1 command + 10 data bytes, rail on, controller re-initialised
 *   recover   stripes again, full refresh */
#include <stdio.h>
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "esp_heap_caps.h"
#include "esp_timer.h"

#define EPD_PWR 6
#define EPD_BUSY 8
#define EPD_RST 9
#define EPD_DC 10
#define EPD_CS 11
#define EPD_SCK 12
#define EPD_MOSI 13
#define FRAME 30000
#define STRIDE 100

static spi_device_handle_t spi;
static uint8_t *frame;

static int64_t wait_idle(void)
{
    int64_t t0 = esp_timer_get_time();
    while (gpio_get_level(EPD_BUSY) == 0) {
        if (esp_timer_get_time() - t0 > 5000000) return -1;
        vTaskDelay(1);
    }
    return esp_timer_get_time() - t0;
}

static void send(int dc, const uint8_t *data, size_t len)
{
    gpio_set_level(EPD_DC, dc);
    gpio_set_level(EPD_CS, 0);
    spi_transaction_t t = {.length = len * 8, .tx_buffer = data};
    ESP_ERROR_CHECK(spi_device_transmit(spi, &t));
    gpio_set_level(EPD_CS, 1);
}

static void cmd(uint8_t c) { send(0, &c, 1); }
static void data1(uint8_t d) { send(1, &d, 1); }

static void step(const char *name, int64_t busy)
{
    printf("DIAG step %s busy_us=%lld\n", name, busy);
    vTaskDelay(pdMS_TO_TICKS(500));
}

static void reset_pulse(void)
{
    gpio_set_level(EPD_RST, 0);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(EPD_RST, 1);
    vTaskDelay(pdMS_TO_TICKS(1));
}

static void init_controller(void)
{
    reset_pulse();
    wait_idle();
    cmd(0x00); data1(0x1f); data1(0x29);   /* panel setting: OTP waveforms */
    cmd(0xE9); data1(0x01); wait_idle();
    cmd(0x50); data1(0x37);                /* full (not transition) refresh */
    cmd(0x04); wait_idle();
}

static int64_t refresh(void)
{
    cmd(0x12); data1(0x00);
    return wait_idle();
}

static void stripes(void)
{
    for (int i = 0; i < FRAME; i++) frame[i] = ((i / STRIDE) % 2) ? 0x00 : 0x55;
}

void app_main(void)
{
    gpio_config_t out = {.pin_bit_mask = (1ULL << 17) | (1ULL << EPD_PWR) | (1ULL << EPD_RST) | (1ULL << EPD_DC) | (1ULL << EPD_CS), .mode = GPIO_MODE_OUTPUT};
    gpio_config(&out);
    gpio_set_level(17, 1);
    gpio_set_level(EPD_CS, 1);
    gpio_set_level(EPD_RST, 1);
    gpio_config_t in = {.pin_bit_mask = 1ULL << EPD_BUSY, .mode = GPIO_MODE_INPUT, .pull_up_en = GPIO_PULLUP_ENABLE};
    gpio_config(&in);
    spi_bus_config_t bus = {.mosi_io_num = EPD_MOSI, .miso_io_num = -1, .sclk_io_num = EPD_SCK, .quadwp_io_num = -1, .quadhd_io_num = -1, .max_transfer_sz = FRAME};
    ESP_ERROR_CHECK(spi_bus_initialize(SPI3_HOST, &bus, SPI_DMA_CH_AUTO));
    spi_device_interface_config_t dev = {.clock_speed_hz = 10 * 1000 * 1000, .mode = 0, .spics_io_num = -1, .queue_size = 1};
    ESP_ERROR_CHECK(spi_bus_add_device(SPI3_HOST, &dev, &spi));
    frame = heap_caps_malloc(FRAME, MALLOC_CAP_DMA);

    gpio_set_level(EPD_PWR, 1);
    vTaskDelay(pdMS_TO_TICKS(10));
    init_controller();

    stripes();
    cmd(0x10); send(1, frame, FRAME);
    step("full", refresh());

    memset(frame, 0x00, 1000);
    cmd(0x10); send(1, frame, 1000);
    step("short", refresh());

    for (int i = 0; i < 535; i++) frame[i] = (uint8_t)(i * 13 + 5);
    cmd(0x20); send(1, frame, 535);
    step("unknown", refresh());
    cmd(0x00); data1(0x1f); data1(0x29);

    memset(frame, 0x55, FRAME);
    cmd(0x10); send(1, frame, FRAME);
    cmd(0x12); data1(0x00);
    vTaskDelay(pdMS_TO_TICKS(20));
    int64_t before = gpio_get_level(EPD_BUSY);
    reset_pulse();
    int64_t after = wait_idle();
    printf("DIAG reset busy_before=%lld\n", before);
    step("reset", after);

    gpio_set_level(EPD_PWR, 0);
    vTaskDelay(pdMS_TO_TICKS(20));
    cmd(0x10); send(1, frame, 10);
    gpio_set_level(EPD_PWR, 1);
    vTaskDelay(pdMS_TO_TICKS(10));
    init_controller();
    step("unpowered", 0);

    stripes();
    cmd(0x10); send(1, frame, FRAME);
    step("recover", refresh());
    printf("DIAG panel done\n");
}
