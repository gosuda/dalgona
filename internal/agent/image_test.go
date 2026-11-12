package agent

import (
	"testing"
)

func TestImageMIME(t *testing.T) {
	tests := []struct {
		name string
		data string
		want string
		err  string
	}{
		{name: "png", data: "\x89PNG\r\n\x1a\n", want: "image/png"},
		{name: "jpeg", data: "\xff\xd8\xff\xe0", want: "image/jpeg"},
		{name: "gif", data: "GIF89a", want: "image/gif"},
		{name: "webp refused", data: "RIFF\x00\x00\x00\x00WEBPVP8 ", err: "unsupported image type image/webp; send PNG, JPEG or GIF"},
		{name: "empty refused", data: "", err: "unsupported image type text/plain; charset=utf-8; send PNG, JPEG or GIF"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := ImageMIME([]byte(tc.data))
			if tc.err != "" {
				if err == nil || err.Error() != tc.err {
					t.Fatalf("ImageMIME error = %v, want %q", err, tc.err)
				}
				return
			}
			if err != nil || got != tc.want {
				t.Fatalf("ImageMIME = %q, %v; want %q", got, err, tc.want)
			}
		})
	}
}

func TestCheckImages(t *testing.T) {
	oneByte := func(n int) []Image {
		images := make([]Image, n)
		for i := range images {
			images[i] = Image{Mime: "image/png", Data: make([]byte, 1)}
		}
		return images
	}
	tests := []struct {
		name   string
		images []Image
		err    string
	}{
		{name: "ten images pass", images: oneByte(10)},
		{name: "eleven images refused", images: oneByte(11), err: "a request takes at most 10 images, got 11"},
		{
			name:   "one image over the limit",
			images: []Image{{Mime: "image/png", Data: make([]byte, 5242881)}},
			err:    "image 1 is 5242881 bytes; the limit is 5242880 bytes (5 MiB)",
		},
		{
			name: "three images over the total",
			images: []Image{
				{Mime: "image/png", Data: make([]byte, 4194304)},
				{Mime: "image/png", Data: make([]byte, 4194304)},
				{Mime: "image/png", Data: make([]byte, 4194304)},
			},
			err: "images total 12582912 bytes; the limit is 10485760 bytes (10 MiB) per request",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			err := CheckImages(tc.images)
			if tc.err == "" {
				if err != nil {
					t.Fatalf("CheckImages = %v, want nil", err)
				}
				return
			}
			if err == nil || err.Error() != tc.err {
				t.Fatalf("CheckImages = %v, want %q", err, tc.err)
			}
		})
	}
}
