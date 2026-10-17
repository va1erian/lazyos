@echo off
rem Runs measure_build.ps1 without changing the machine's execution policy.
rem Example: tools\buildbench\measure_build.cmd -Label "Ryzen 9 9950X" -Runs 3
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0measure_build.ps1" %*
