#!/usr/bin/env node
"use strict";

const { install } = require("./lib/release");

install().catch((error) => {
  console.error(`argus: ${error.message}`);
  process.exit(1);
});
