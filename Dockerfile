FROM python:3.11-slim-bullseye

ENV PYTHONDONTWRITEBYTECODE=1
ENV PYTHONUNBUFFERED=1

COPY requirements.txt .
RUN python -m pip install --no-cache-dir --upgrade -r requirements.txt

WORKDIR /app
COPY . /app

RUN set -eux; \
	apt-get update; \
	apt-get install -y gosu; \
	rm -rf /var/lib/apt/lists/*; \
# verify that the binary works
	gosu nobody true

RUN chmod a+x docker-entrypoint.sh

ENTRYPOINT ["/app/docker-entrypoint.sh"]

CMD ["python", "main.py"]
