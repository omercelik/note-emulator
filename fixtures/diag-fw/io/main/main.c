/* GPIO-interrupt and SPI3-DMA diagnostic for the NOTE Emulator (fixtures/diag-fw/io).
 * INPUT-01: UP (GPIO39) and DOWN (GPIO18) are negative-edge interrupts through the IDF ISR
 * service; each count change prints "DIAG gpio up=N down=M".
 * SPI-01: one 30000-byte SPI3 DMA transaction (more than one 4092-byte descriptor) writes a
 * known pattern to the SSD2683 RAM on the NOTE wiring, then refreshes; "DIAG spi done". */
#include <stdio.h>
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "esp_heap_caps.h"

#define PIN_UP 39
#define PIN_DOWN 18
#define EPD_PWR 6
#define EPD_BUSY 8
#define EPD_RST 9
#define EPD_DC 10
#define EPD_CS 11
#define EPD_SCK 12
#define EPD_MOSI 13
#define FRAME 30000

static volatile int up_count, down_count;

static void IRAM_ATTR on_edge(void *arg)
{
    if ((int)arg == PIN_UP) up_count++; else down_count++;
}

static spi_device_handle_t spi;

static void wait_idle(void)
{
    while (gpio_get_level(EPD_BUSY) == 0) vTaskDelay(1);
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

static void spi_test(void)
{
    gpio_config_t out = {.pin_bit_mask = (1ULL << 17) | (1ULL << EPD_PWR) | (1ULL << EPD_RST) | (1ULL << EPD_DC) | (1ULL << EPD_CS), .mode = GPIO_MODE_OUTPUT};
    gpio_config(&out);
    gpio_set_level(17, 1);                 /* battery latch */
    gpio_config_t in = {.pin_bit_mask = 1ULL << EPD_BUSY, .mode = GPIO_MODE_INPUT, .pull_up_en = GPIO_PULLUP_ENABLE};
    gpio_config(&in);
    gpio_set_level(EPD_CS, 1);
    spi_bus_config_t bus = {.mosi_io_num = EPD_MOSI, .miso_io_num = -1, .sclk_io_num = EPD_SCK, .quadwp_io_num = -1, .quadhd_io_num = -1, .max_transfer_sz = FRAME};
    ESP_ERROR_CHECK(spi_bus_initialize(SPI3_HOST, &bus, SPI_DMA_CH_AUTO));
    spi_device_interface_config_t dev = {.clock_speed_hz = 10 * 1000 * 1000, .mode = 0, .spics_io_num = -1, .queue_size = 1};
    ESP_ERROR_CHECK(spi_bus_add_device(SPI3_HOST, &dev, &spi));
    gpio_set_level(EPD_PWR, 1);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(EPD_RST, 0);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(EPD_RST, 1);
    vTaskDelay(pdMS_TO_TICKS(10));
    wait_idle();
    cmd(0xE9); data1(0x01); wait_idle();
    uint8_t *frame = heap_caps_malloc(FRAME, MALLOC_CAP_DMA);
    for (int i = 0; i < FRAME; i++) frame[i] = (uint8_t)(i * 37 + 11);
    cmd(0x10);
    send(1, frame, FRAME);                 /* one transaction, chained descriptors */
    cmd(0x04); wait_idle();
    cmd(0x12); data1(0x00); vTaskDelay(pdMS_TO_TICKS(10)); wait_idle();
    cmd(0x02); data1(0x00); wait_idle();
    printf("DIAG spi done\n");
}

void app_main(void)
{
    gpio_config_t btn = {.pin_bit_mask = (1ULL << PIN_UP) | (1ULL << PIN_DOWN), .mode = GPIO_MODE_INPUT,
                         .pull_up_en = GPIO_PULLUP_ENABLE, .intr_type = GPIO_INTR_NEGEDGE};
    gpio_config(&btn);
    gpio_install_isr_service(0);
    gpio_isr_handler_add(PIN_UP, on_edge, (void *)PIN_UP);
    gpio_isr_handler_add(PIN_DOWN, on_edge, (void *)PIN_DOWN);
    spi_test();
    printf("DIAG gpio ready\n");
    int last_up = -1, last_down = -1;
    for (;;) {
        if (up_count != last_up || down_count != last_down) {
            last_up = up_count; last_down = down_count;
            printf("DIAG gpio up=%d down=%d\n", last_up, last_down);
        }
        vTaskDelay(pdMS_TO_TICKS(20));
    }
}
