### Outside checks of the protocol workstream

- Check: Call the live devin model list (GetCliModelConfigs) with a real account and read supportsImages for an image-capable model. Guards: Step 11. Why it is outside: Needs a live provider account, which this run cannot reach. Fallback: If no listed model reports supportsImages true, Images stays false for every model and inputModalities stays ["text"]; no other change is needed.

