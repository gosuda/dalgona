def load(path):
    return path


class Store:
    def get(self, key):
        def inner():
            return key
        return inner()

@cache
def fetch(url):
    return url
