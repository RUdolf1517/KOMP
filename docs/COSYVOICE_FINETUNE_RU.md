# Дообучение голоса CosyVoice для KOMP

Текущий профиль `cave` использует одну запись как zero-shot пример. Архив уже принес пользу, но остальные записи начнут влиять на голос только после fine-tune. В подготовленном наборе KOMP сейчас 57 пар WAV/TXT: этого достаточно для эксперимента, но мало для гарантированного результата.

## Машина

Рекомендуется Ubuntu 22.04/24.04, NVIDIA GPU с 24 ГБ VRAM, свежий драйвер и CUDA-совместимый PyTorch. На 16 ГБ обучение может потребовать уменьшения `max_frames_in_batch`. На macOS CPU запускать обучение 0.5B-модели не стоит.

Не обучайте поверх рабочей модели. Перенесите на GPU-машину весь репозиторий KOMP без каталогов `target` и `.git`; нужны `vendor/cosyvoice/source`, `vendor/cosyvoice/models/Fun-CosyVoice3-0.5B` и `vendor/cosyvoice/datasets/cave_prepared`.

## 1. Окружение

Из корня KOMP:

```bash
export KOMP_COSYVOICE_PYTHON=python3.10
./scripts/setup-cosyvoice.sh
source vendor/cosyvoice/.venv/bin/activate
python -c "import torch; print(torch.cuda.is_available(), torch.cuda.get_device_name(0))"
```

Последняя команда должна вывести `True` и название GPU. Если выводится `False`, обучение не начинайте: сначала установите CUDA-сборку PyTorch, совместимую с драйвером машины.

## 2. Train/dev набор

```bash
python scripts/prepare-cosyvoice-finetune.py \
  vendor/cosyvoice/datasets/cave_prepared \
  vendor/cosyvoice/finetune/cave/data \
  --speaker cave --dev-count 6
```

Получится 51 запись для обучения и 6 для проверки. Скрипт создаёт `wav.scp`, `text`, `utt2spk`, `spk2utt` и `instruct` в формате CosyVoice.

## 3. Признаки, токены и parquet

```bash
cd vendor/cosyvoice/source
export PYTHONPATH="$PWD:$PWD/third_party/Matcha-TTS"
export TOKENIZERS_PARALLELISM=false
MODEL="$PWD/../models/Fun-CosyVoice3-0.5B"
FT="$PWD/../finetune/cave"

for SPLIT in train dev; do
  python tools/extract_embedding.py --dir "$FT/data/$SPLIT" --onnx_path "$MODEL/campplus.onnx"
  python tools/extract_speech_token.py --dir "$FT/data/$SPLIT" --onnx_path "$MODEL/speech_tokenizer_v3.onnx"
  mkdir -p "$FT/data/$SPLIT/parquet"
  python tools/make_parquet_list.py --num_utts_per_parquet 1000 --num_processes 1 \
    --src_dir "$FT/data/$SPLIT" --des_dir "$FT/data/$SPLIT/parquet"
done
```

## 4. Конфигурация SFT

```bash
cp examples/libritts/cosyvoice3/conf/cosyvoice3.yaml "$FT/cosyvoice3-cave.yaml"
```

В копии конфигурации проверьте следующие значения:

```yaml
padding:
    use_spk_embedding: True

train_conf:
    optim_conf:
        lr: 1e-5
    max_epoch: 10
    accum_grad: 4
    save_per_step: -1
```

Начинайте только с `llm`. Обучать `flow` и `hifigan` на 57 фразах рискованно: голос легче переобучить и испортить произношение.

## 5. Обучение

```bash
torchrun --standalone --nnodes=1 --nproc_per_node=1 cosyvoice/bin/train.py \
  --train_engine torch_ddp \
  --config "$FT/cosyvoice3-cave.yaml" \
  --train_data "$FT/data/train/parquet/data.list" \
  --cv_data "$FT/data/dev/parquet/data.list" \
  --qwen_pretrain_path "$MODEL/CosyVoice-BlankEN" \
  --onnx_path "$MODEL" \
  --model llm \
  --checkpoint "$MODEL/llm.pt" \
  --model_dir "$FT/exp/llm" \
  --tensorboard_dir "$FT/tensorboard" \
  --ddp.dist_backend nccl \
  --num_workers 2 --prefetch 20 --pin_memory --use_amp
```

Следите за validation loss через TensorBoard:

```bash
tensorboard --logdir "$FT/tensorboard" --bind_all
```

Если validation loss растёт несколько эпох подряд, остановите обучение: модель уже переобучается.

## 6. Выбор чекпойнта и возврат в KOMP

В `$FT/exp/llm` появятся `epoch_*_whole.pt` и YAML с validation loss. Следующая команда автоматически выберет эпоху с минимальным validation loss и удалит служебные поля чекпойнта:

```bash
python cosyvoice/bin/average_model.py \
  --dst_model "$FT/exp/llm/cave-llm.pt" \
  --src_path "$FT/exp/llm" --num 1 --val_best

cp -a "$MODEL" "$PWD/../models/Fun-CosyVoice3-0.5B-cave"
cp "$FT/exp/llm/cave-llm.pt" "$PWD/../models/Fun-CosyVoice3-0.5B-cave/llm.pt"
```

Перенесите каталог `Fun-CosyVoice3-0.5B-cave` обратно в `vendor/cosyvoice/models/` основного компьютера и задайте в `komp.prototype.toml`:

```toml
[tts]
model_path = "vendor/cosyvoice/models/Fun-CosyVoice3-0.5B-cave"
```

После перезапуска KOMP сравните одинаковые 10 тестовых фраз на исходной и дообученной моделях. Исходную модель не удаляйте до завершения сравнения.

## Практическое ожидание

Fine-tune улучшает устойчивость тембра, но не ускоряет CPU-синтез. Для заметно более уверенного результата лучше 30–60 минут чистой речи с точными расшифровками, разными фонемами и без музыки. Текущие 57 фраз подходят для первого контролируемого эксперимента.
