# Waveshare ESP32-S3-CAM-OV5640 · waveshare-autopling in the emulator

`run-autopling.sh` boots the unmodified `waveshare-autopling` image (ESP-IDF 5.5, esp_video, esp-dl
pedestrian detector, ES8311 speaker) on `--board waveshare-cam` with a picture on the camera:

    ./run-autopling.sh                      # pedestrians.jpg, 10 s headless -> autopling.wav
    ./run-autopling.sh me.jpg --web 8767    # live UI: http://127.0.0.1:8767/ (picture upload / webcam)

Expected: `ov5640: Detected Camera sensor PID=0x5640`, `pedestrian detector warm-up ok`, camera frames at
10 fps, then `pedestrian detected score=0.83` → `playing pling tone` every 1.5 s (the firmware's cooldown),
audible in the UI and in `autopling.wav`. WiFi is not emulated: core 0 sits in the PHY calibration loop, the
detector/pling loop runs on core 1 regardless.

`pedestrians.jpg`: "Pedestrians using crosswalk (Unsplash)", Wikimedia Commons, CC0.
