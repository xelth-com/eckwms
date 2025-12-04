// API endpoint для AI-доступной документации MovFast
// Добавить в ваш Express сервер

const express = require('express');
const fs = require('fs').promises;
const path = require('path');

const docsRouter = express.Router();

// Структура документации для AI
const docsStructure = {
  "movfast-ubuntu-adb": {
    title: "MovFast USB Debugging Setup for Ubuntu",
    category: "troubleshooting",
    tags: ["ubuntu", "adb", "usb", "linux", "mediatek"],
    difficulty: "intermediate",
    timeToFix: "2-5 minutes",
    platforms: ["Ubuntu 20.04+", "Debian", "Linux Mint"],
    devices: ["MovFast MT15", "Ranger2", "MediaTek-based"],
    file: "MovFast-Ubuntu-ADB-Setup-Guide.md"
  }
};

// 1. API: Поиск документации
docsRouter.get('/api/docs/search', async (req, res) => {
  const { q, platform, category } = req.query;

  // Простой поиск по тегам и заголовкам
  const results = Object.entries(docsStructure)
    .filter(([key, doc]) => {
      const matchesQuery = !q ||
        doc.title.toLowerCase().includes(q.toLowerCase()) ||
        doc.tags.some(tag => tag.includes(q.toLowerCase()));

      const matchesPlatform = !platform ||
        doc.platforms.some(p => p.toLowerCase().includes(platform.toLowerCase()));

      const matchesCategory = !category || doc.category === category;

      return matchesQuery && matchesPlatform && matchesCategory;
    })
    .map(([key, doc]) => ({
      id: key,
      title: doc.title,
      category: doc.category,
      difficulty: doc.difficulty,
      timeToFix: doc.timeToFix,
      url: `/api/docs/${key}`
    }));

  res.json({
    query: q,
    count: results.length,
    results
  });
});

// 2. API: Получить конкретный документ
docsRouter.get('/api/docs/:docId', async (req, res) => {
  const { docId } = req.params;
  const doc = docsStructure[docId];

  if (!doc) {
    return res.status(404).json({ error: 'Documentation not found' });
  }

  try {
    // Читаем markdown файл
    const filePath = path.join(__dirname, '../../', doc.file);
    const content = await fs.readFile(filePath, 'utf-8');

    // Парсим для структурированного вывода
    const structured = parseMarkdownToStructured(content);

    res.json({
      id: docId,
      title: doc.title,
      category: doc.category,
      tags: doc.tags,
      platforms: doc.platforms,
      devices: doc.devices,
      metadata: {
        difficulty: doc.difficulty,
        estimatedTime: doc.timeToFix
      },
      content: {
        markdown: content,
        structured: structured
      },
      format: req.query.format || 'json' // поддержка ?format=markdown
    });
  } catch (error) {
    res.status(500).json({ error: 'Failed to load documentation' });
  }
});

// 3. API: Структурированные шаги решения
docsRouter.get('/api/docs/:docId/steps', async (req, res) => {
  const { docId } = req.params;

  // Пример для ubuntu-adb
  if (docId === 'movfast-ubuntu-adb') {
    res.json({
      problem: {
        description: "ADB not detecting MovFast device on Ubuntu",
        symptoms: [
          "Empty device list in 'adb devices'",
          "Device showing as '?????? no permissions'",
          "No authorization dialog on device"
        ]
      },
      solution: {
        estimatedTime: "2-5 minutes",
        steps: [
          {
            order: 1,
            title: "Identify Device",
            command: "lsusb",
            expectedOutput: "ID 0e8d:201c MediaTek Inc. Ranger2",
            explanation: "Check if Ubuntu detects the device at USB level",
            type: "diagnostic"
          },
          {
            order: 2,
            title: "Create USB Permission Rule",
            command: "echo 'SUBSYSTEM==\"usb\", ATTR{idVendor}==\"0e8d\", MODE=\"0666\", GROUP=\"plugdev\"' | sudo tee /etc/udev/rules.d/51-android.rules",
            explanation: "Grant USB access permissions for MediaTek devices",
            requiresSudo: true,
            type: "configuration"
          },
          {
            order: 3,
            title: "Apply Rules",
            commands: [
              "sudo udevadm control --reload-rules",
              "sudo service udev restart"
            ],
            explanation: "Reload USB device rules",
            requiresSudo: true,
            type: "system"
          },
          {
            order: 4,
            title: "Restart ADB",
            commands: [
              "adb kill-server",
              "adb start-server"
            ],
            explanation: "Restart ADB to recognize new permissions",
            requiresSudo: false,
            type: "tool"
          },
          {
            order: 5,
            title: "Configure Device USB Mode",
            manual: true,
            instructions: [
              "Disconnect and reconnect USB cable",
              "On device: swipe down notification panel",
              "Tap 'USB charging' or 'USB options'",
              "Select 'File Transfer (MTP)' or 'PTP (Camera)'"
            ],
            explanation: "Activate ADB interface on the device",
            type: "device-config"
          },
          {
            order: 6,
            title: "Authorize Computer",
            manual: true,
            instructions: [
              "Wait for dialog on device screen",
              "Check 'Always allow from this computer'",
              "Tap OK"
            ],
            explanation: "Save computer's RSA key on device",
            type: "authorization"
          },
          {
            order: 7,
            title: "Verify Connection",
            command: "adb devices",
            expectedOutput: "MT15AEM24120007    device",
            explanation: "Confirm device is detected and authorized",
            type: "verification"
          }
        ]
      },
      troubleshooting: [
        {
          issue: "Still showing 'no permissions'",
          solution: {
            commands: [
              "rm ~/.android/adbkey*",
              "adb kill-server",
              "adb start-server"
            ],
            explanation: "Clear old authorization keys"
          }
        },
        {
          issue: "No authorization dialog appears",
          solution: {
            manual: true,
            steps: [
              "Settings → Developer Options",
              "Tap 'Revoke USB debugging authorizations'",
              "Toggle USB debugging OFF then ON",
              "Reconnect cable"
            ]
          }
        }
      ]
    });
  } else {
    res.status(404).json({ error: 'Steps not found for this document' });
  }
});

// 4. API: Список всех доступных документов
docsRouter.get('/api/docs', (req, res) => {
  const docsList = Object.entries(docsStructure).map(([key, doc]) => ({
    id: key,
    title: doc.title,
    category: doc.category,
    difficulty: doc.difficulty,
    platforms: doc.platforms,
    url: `/api/docs/${key}`
  }));

  res.json({
    count: docsList.length,
    documents: docsList
  });
});

// Вспомогательная функция: парсинг Markdown в структуру
function parseMarkdownToStructured(markdown) {
  // Простой парсер - можно улучшить
  const sections = markdown.split(/^##\s+/m).filter(s => s.trim());

  return sections.map(section => {
    const [title, ...content] = section.split('\n');
    return {
      title: title.trim(),
      content: content.join('\n').trim()
    };
  });
}

module.exports = docsRouter;

// Использование в основном сервере:
// const docsRouter = require('./docs/api-documentation-example');
// app.use(docsRouter);
