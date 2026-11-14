// Cast relay remuxer (cast/relay.rs): reads a provider stream with the
// embedded FFmpeg and writes it as fragmented MP4 — the progressive format a
// Chromecast's Default Media Receiver plays. Video is copied (the receiver
// decodes H.264/HEVC/VP9/AV1 itself); audio is copied when it is AAC or MP3,
// anything else (AC-3, E-AC-3, MP2, DTS, Opus, …) becomes stereo AAC.
// One text subtitle stream (SubRip, ASS, mov_text, WebVTT) can be decoded on
// the way and handed out as cues (`cue`), which the relay serves as WebVTT
// next to the MP4; picture subtitles can't be cast. Only that stream is read:
// MP4 inputs would otherwise seek back and forth for every other one.
//
// One call = one HTTP response: tp_remux_run blocks until the input ends,
// `aborted` says stop, or `write` refuses more data (the TV hung up).

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavcodec/bsf.h>
#include <libavcodec/codec_desc.h>
#include <libavformat/avformat.h>
#include <libavutil/audio_fifo.h>
#include <libavutil/avstring.h>
#include <libavutil/channel_layout.h>
#include <libavutil/opt.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>

// One audio or subtitle stream of the input, reported before streaming.
typedef struct tp_stream {
    int index;          // input stream index
    int kind;           // 1 audio, 2 subtitle
    const char *codec;  // "eac3", "subrip", …
    const char *lang;   // NULL: unknown
    const char *title;  // NULL: none
    int channels;       // audio only
    int is_default;     // the file marks it as the default
    int castable;       // audio: always; subtitles: text ones (not on live input)
    int selected;       // the audio stream this run streams / the subtitle stream it cues
} tp_stream;

typedef struct tp_remux {
    // input
    const char *url;
    const char *user_agent; // NULL: FFmpeg's default
    const char *referrer;   // NULL: none
    double start;           // seconds into the input, <= 0: from the start
    const char *audio_lang; // preferred audio languages, comma-separated ("eng,en"), or NULL
    int live;               // repair timestamp jumps, never seek
    int audio_index;        // >= 0: stream this input audio stream; < 0: pick by audio_lang
    int sub_index;          // >= 0: cue this subtitle stream; -1: pick by sub_lang; -2: none
    const char *sub_lang;   // preferred subtitle languages, like audio_lang, or NULL
    // callbacks
    void *opaque;
    int (*write)(void *opaque, const uint8_t *buf, int len); // < 0: stop
    int (*aborted)(void *opaque);                             // != 0: stop
    // once, before the first packet: input duration (0 = unknown) and where
    // the output really starts (the keyframe before `start`)
    void (*started)(void *opaque, double duration, double first);
    // once per audio/subtitle stream, after opening and before `started`
    void (*stream)(void *opaque, const tp_stream *s);
    // a subtitle event of input stream `index`: seconds on the input's clock
    // (0 = its start), text as an ASS dialogue line
    void (*cue)(void *opaque, int index, double start, double end, const char *ass);
    // output
    char error[256];
} tp_remux;

typedef struct {
    tp_remux *o;
    AVFormatContext *ic, *oc;
    int vin, ain;   // input stream indexes, -1 if absent
    int vout, aout; // output stream indexes
    // copied AAC from TS comes with ADTS headers, which MP4 can't carry
    AVBSFContext *adts;
    // audio transcoding (dec == NULL: copied)
    AVCodecContext *dec, *enc;
    SwrContext *swr;
    int swr_fmt, swr_rate;
    AVChannelLayout swr_layout;
    AVAudioFifo *fifo;
    int64_t next_apts; // encoder time base (1/sample_rate), AV_NOPTS_VALUE: unset
    AVFrame *frame, *eframe;
    AVPacket *pkt;
    // timestamps
    int64_t offset;        // live: correction added to every input packet (AV_TIME_BASE)
    int64_t last_in[2];    // last corrected input dts per kind (0 video, 1 audio)
    int64_t last_out[2];   // last written dts per output stream (its time base)
    int started;
    // the subtitle stream handed out as cues (-1: none) and its decoder
    int sin;
    AVCodecContext *sdec;
} remux;

static void fail(tp_remux *o, const char *what, int err) {
    if (o->error[0]) return;
    char msg[AV_ERROR_MAX_STRING_SIZE] = {0};
    if (err) av_strerror(err, msg, sizeof msg);
    snprintf(o->error, sizeof o->error, "%s%s%s", what, err ? ": " : "", msg);
}

static int interrupt_cb(void *opaque) {
    tp_remux *o = opaque;
    return o->aborted(o->opaque);
}

static int write_cb(void *opaque, const uint8_t *buf, int len) {
    tp_remux *o = opaque;
    return o->write(o->opaque, buf, len) < 0 ? AVERROR_EXIT : len;
}

/// Video the receiver decodes itself.
static int castable_video(enum AVCodecID id) {
    return id == AV_CODEC_ID_H264 || id == AV_CODEC_ID_HEVC || id == AV_CODEC_ID_VP9 || id == AV_CODEC_ID_AV1;
}

static int copyable_audio(enum AVCodecID id) {
    return id == AV_CODEC_ID_AAC || id == AV_CODEC_ID_MP3;
}

/// Writes one packet (output time base, stream already set): dts must keep
/// increasing for the MP4 muxer.
static int mux(remux *r, AVPacket *pkt, int kind) {
    if (pkt->dts != AV_NOPTS_VALUE) {
        if (r->last_out[kind] != AV_NOPTS_VALUE && pkt->dts <= r->last_out[kind]) {
            pkt->dts = r->last_out[kind] + 1;
            if (pkt->pts != AV_NOPTS_VALUE && pkt->pts < pkt->dts) pkt->pts = pkt->dts;
        }
        r->last_out[kind] = pkt->dts;
    }
    int ret = av_interleaved_write_frame(r->oc, pkt);
    if (ret < 0) fail(r->o, ret == AVERROR_EXIT ? "the TV closed the stream" : "cannot write", ret == AVERROR_EXIT ? 0 : ret);
    return ret;
}

// ------------------------------------------------------------- audio

static int open_adts_filter(remux *r, AVStream *in, AVStream *out) {
    const AVBitStreamFilter *f = av_bsf_get_by_name("aac_adtstoasc");
    int ret = f ? av_bsf_alloc(f, &r->adts) : AVERROR_BSF_NOT_FOUND;
    if (ret >= 0) ret = avcodec_parameters_copy(r->adts->par_in, in->codecpar);
    if (ret >= 0) {
        r->adts->time_base_in = in->time_base;
        ret = av_bsf_init(r->adts);
    }
    if (ret >= 0) ret = avcodec_parameters_copy(out->codecpar, r->adts->par_out);
    if (ret < 0) fail(r->o, "cannot prepare the AAC audio", ret);
    out->codecpar->codec_tag = 0;
    return ret;
}

/// Copied AAC: through aac_adtstoasc (a no-op for raw AAC), then out.
static int copy_aac(remux *r, AVPacket *pkt) {
    AVStream *in = r->ic->streams[r->ain], *out = r->oc->streams[r->aout];
    int ret = av_bsf_send_packet(r->adts, pkt);
    if (ret < 0) return 0; // a broken packet: skip it
    while ((ret = av_bsf_receive_packet(r->adts, r->pkt)) >= 0) {
        r->pkt->stream_index = r->aout;
        r->pkt->pos = -1;
        av_packet_rescale_ts(r->pkt, in->time_base, out->time_base);
        ret = mux(r, r->pkt, 1);
        av_packet_unref(r->pkt);
        if (ret < 0) return ret;
    }
    return ret == AVERROR(EAGAIN) || ret == AVERROR_EOF ? 0 : ret;
}

static int open_audio_transcoder(remux *r, AVStream *in, AVStream *out) {
    const AVCodec *dcodec = avcodec_find_decoder(in->codecpar->codec_id);
    const AVCodec *ecodec = avcodec_find_encoder(AV_CODEC_ID_AAC);
    if (!dcodec || !ecodec) {
        fail(r->o, "no decoder for this audio", 0);
        return AVERROR_DECODER_NOT_FOUND;
    }
    r->dec = avcodec_alloc_context3(dcodec);
    avcodec_parameters_to_context(r->dec, in->codecpar);
    r->dec->pkt_timebase = in->time_base;
    int ret = avcodec_open2(r->dec, dcodec, NULL);
    if (ret < 0) {
        fail(r->o, "cannot decode the audio", ret);
        return ret;
    }

    int rate = in->codecpar->sample_rate;
    r->enc = avcodec_alloc_context3(ecodec);
    r->enc->sample_fmt = AV_SAMPLE_FMT_FLTP;
    r->enc->sample_rate = rate == 44100 || rate == 48000 ? rate : 48000;
    av_channel_layout_default(&r->enc->ch_layout, 2);
    r->enc->bit_rate = 192000;
    r->enc->time_base = (AVRational){1, r->enc->sample_rate};
    if (r->oc->oformat->flags & AVFMT_GLOBALHEADER) r->enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    ret = avcodec_open2(r->enc, ecodec, NULL);
    if (ret < 0) {
        fail(r->o, "cannot encode AAC", ret);
        return ret;
    }
    avcodec_parameters_from_context(out->codecpar, r->enc);
    out->time_base = r->enc->time_base;

    r->fifo = av_audio_fifo_alloc(AV_SAMPLE_FMT_FLTP, 2, r->enc->frame_size * 4);
    r->frame = av_frame_alloc();
    r->eframe = av_frame_alloc();
    r->next_apts = AV_NOPTS_VALUE;
    r->swr_fmt = -1;
    return 0;
}

/// (Re)creates the resampler when the decoded format changes — TV channels
/// switch between 5.1 and stereo at programme boundaries.
static int ensure_swr(remux *r, const AVFrame *f) {
    if (r->swr && f->format == r->swr_fmt && f->sample_rate == r->swr_rate &&
        !av_channel_layout_compare(&f->ch_layout, &r->swr_layout))
        return 0;
    swr_free(&r->swr);
    av_channel_layout_uninit(&r->swr_layout);
    int ret = swr_alloc_set_opts2(&r->swr, &r->enc->ch_layout, r->enc->sample_fmt, r->enc->sample_rate,
                                  &f->ch_layout, f->format, f->sample_rate, 0, NULL);
    if (ret >= 0) ret = swr_init(r->swr);
    if (ret < 0) {
        fail(r->o, "cannot convert the audio", ret);
        return ret;
    }
    r->swr_fmt = f->format;
    r->swr_rate = f->sample_rate;
    av_channel_layout_copy(&r->swr_layout, &f->ch_layout);
    return 0;
}

static int encode_and_mux(remux *r, AVFrame *f) {
    int ret = avcodec_send_frame(r->enc, f);
    if (ret < 0 && ret != AVERROR_EOF) return ret;
    AVStream *out = r->oc->streams[r->aout];
    while ((ret = avcodec_receive_packet(r->enc, r->pkt)) >= 0) {
        r->pkt->stream_index = r->aout;
        av_packet_rescale_ts(r->pkt, r->enc->time_base, out->time_base);
        ret = mux(r, r->pkt, 1);
        av_packet_unref(r->pkt);
        if (ret < 0) return ret;
    }
    return ret == AVERROR(EAGAIN) || ret == AVERROR_EOF ? 0 : ret;
}

/// Sends whole encoder frames from the FIFO (all of it when `flush`).
static int drain_fifo(remux *r, int flush) {
    int fs = r->enc->frame_size;
    while (av_audio_fifo_size(r->fifo) >= fs || (flush && av_audio_fifo_size(r->fifo) > 0)) {
        int n = FFMIN(fs, av_audio_fifo_size(r->fifo));
        AVFrame *e = r->eframe;
        av_frame_unref(e);
        e->nb_samples = n;
        e->format = r->enc->sample_fmt;
        e->sample_rate = r->enc->sample_rate;
        av_channel_layout_copy(&e->ch_layout, &r->enc->ch_layout);
        int ret = av_frame_get_buffer(e, 0);
        if (ret < 0) return ret;
        av_audio_fifo_read(r->fifo, (void **)e->data, n);
        e->pts = r->next_apts;
        r->next_apts += n;
        ret = encode_and_mux(r, e);
        if (ret < 0) return ret;
    }
    return 0;
}

static int transcode_audio(remux *r, AVPacket *pkt) {
    int ret = avcodec_send_packet(r->dec, pkt);
    // a broken packet in a live stream: skip it, keep going
    if (ret < 0 && ret != AVERROR(EAGAIN) && ret != AVERROR_EOF) return 0;
    while ((ret = avcodec_receive_frame(r->dec, r->frame)) >= 0) {
        AVFrame *f = r->frame;
        if ((ret = ensure_swr(r, f)) < 0) return ret;
        int64_t ts = f->best_effort_timestamp;
        if (ts != AV_NOPTS_VALUE) {
            int64_t pts = av_rescale_q(ts, r->dec->pkt_timebase, r->enc->time_base);
            int64_t expected = r->next_apts + av_audio_fifo_size(r->fifo);
            // first frame, or audio jumped: follow the input's clock
            if (r->next_apts == AV_NOPTS_VALUE || llabs(pts - expected) > r->enc->sample_rate / 2)
                r->next_apts = pts - av_audio_fifo_size(r->fifo);
        } else if (r->next_apts == AV_NOPTS_VALUE) {
            r->next_apts = 0;
        }
        int cap = swr_get_out_samples(r->swr, f->nb_samples);
        uint8_t **buf = NULL;
        if ((ret = av_samples_alloc_array_and_samples(&buf, NULL, 2, cap, r->enc->sample_fmt, 0)) < 0) return ret;
        int n = swr_convert(r->swr, buf, cap, (const uint8_t **)f->extended_data, f->nb_samples);
        if (n > 0) av_audio_fifo_write(r->fifo, (void **)buf, n);
        av_freep(&buf[0]);
        av_freep(&buf);
        av_frame_unref(f);
        if (n < 0) return n;
        if ((ret = drain_fifo(r, 0)) < 0) return ret;
    }
    return ret == AVERROR(EAGAIN) || ret == AVERROR_EOF ? 0 : ret;
}

// ------------------------------------------------------------- timestamps

/// Live TV: streams restart their clock (ads, encoder restarts); like the
/// ffmpeg tool, a jump of more than 10 s moves everything back in line.
static void repair_live(remux *r, AVPacket *pkt, int kind) {
    AVRational tb = r->ic->streams[pkt->stream_index]->time_base;
    int64_t dts = pkt->dts != AV_NOPTS_VALUE ? pkt->dts : pkt->pts;
    if (dts == AV_NOPTS_VALUE) return;
    int64_t d = av_rescale_q(dts, tb, AV_TIME_BASE_Q) + r->offset;
    int64_t last = r->last_in[kind];
    if (last != AV_NOPTS_VALUE && (d < last - 10 * AV_TIME_BASE || d > last + 10 * AV_TIME_BASE)) {
        r->offset += last + AV_TIME_BASE / 25 - d;
        d = last + AV_TIME_BASE / 25;
    }
    r->last_in[kind] = d;
    int64_t shift = av_rescale_q(r->offset, AV_TIME_BASE_Q, tb);
    if (pkt->pts != AV_NOPTS_VALUE) pkt->pts += shift;
    if (pkt->dts != AV_NOPTS_VALUE) pkt->dts += shift;
}

static void report_start(remux *r, const AVPacket *pkt) {
    if (r->started) return;
    r->started = 1;
    double duration = !r->o->live && r->ic->duration > 0 ? r->ic->duration / (double)AV_TIME_BASE : 0;
    double first = 0;
    int64_t ts = pkt->pts != AV_NOPTS_VALUE ? pkt->pts : pkt->dts;
    if (!r->o->live && ts != AV_NOPTS_VALUE) {
        first = ts * av_q2d(r->ic->streams[pkt->stream_index]->time_base);
        if (r->ic->start_time != AV_NOPTS_VALUE) first -= r->ic->start_time / (double)AV_TIME_BASE;
        if (first < 0) first = 0;
    }
    if (r->o->started) r->o->started(r->o->opaque, duration, first);
}

// ------------------------------------------------------------- setup

/// The first audio stream in the viewer's language (mpv's `alang` order).
static int preferred_audio(AVFormatContext *ic, const char *langs) {
    if (!langs || !*langs) return -1;
    char want[64];
    for (const char *p = langs; *p;) {
        size_t n = strcspn(p, ",");
        if (n > 0 && n < sizeof want) {
            memcpy(want, p, n);
            want[n] = 0;
            for (unsigned i = 0; i < ic->nb_streams; i++) {
                AVStream *st = ic->streams[i];
                const AVDictionaryEntry *lang = av_dict_get(st->metadata, "language", NULL, 0);
                if (st->codecpar->codec_type == AVMEDIA_TYPE_AUDIO && lang && !av_strcasecmp(lang->value, want))
                    return (int)i;
            }
        }
        p += n;
        while (*p == ',' || *p == ' ') p++;
    }
    return -1;
}

// ------------------------------------------------------------- subtitles

/// Subtitles that decode to text (and so can become WebVTT).
static int text_subtitle(enum AVCodecID id) {
    const AVCodecDescriptor *d = avcodec_descriptor_get(id);
    return d && (d->props & AV_CODEC_PROP_TEXT_SUB);
}

static AVCodecContext *open_subtitle_decoder(AVStream *st) {
    const AVCodec *codec = avcodec_find_decoder(st->codecpar->codec_id);
    if (!codec) return NULL;
    AVCodecContext *dec = avcodec_alloc_context3(codec);
    if (!dec) return NULL;
    if (avcodec_parameters_to_context(dec, st->codecpar) < 0 || (dec->pkt_timebase = st->time_base, avcodec_open2(dec, codec, NULL) < 0))
        avcodec_free_context(&dec);
    return dec;
}

static const char *tag(const AVStream *st, const char *key) {
    const AVDictionaryEntry *e = av_dict_get(st->metadata, key, NULL, 0);
    return e && e->value[0] ? e->value : NULL;
}

/// The first text subtitle stream in the viewer's languages, else the one
/// the file marks as default (like mpv's `slang`).
static int preferred_subtitle(AVFormatContext *ic, const char *langs) {
    char want[64];
    for (const char *p = langs ? langs : ""; *p;) {
        size_t n = strcspn(p, ",");
        if (n > 0 && n < sizeof want) {
            memcpy(want, p, n);
            want[n] = 0;
            for (unsigned i = 0; i < ic->nb_streams; i++) {
                AVStream *st = ic->streams[i];
                const char *lang = tag(st, "language");
                if (st->codecpar->codec_type == AVMEDIA_TYPE_SUBTITLE && text_subtitle(st->codecpar->codec_id) && lang &&
                    !av_strcasecmp(lang, want))
                    return (int)i;
            }
        }
        p += n;
        while (*p == ',' || *p == ' ') p++;
    }
    for (unsigned i = 0; i < ic->nb_streams; i++) {
        AVStream *st = ic->streams[i];
        if (st->codecpar->codec_type == AVMEDIA_TYPE_SUBTITLE && text_subtitle(st->codecpar->codec_id) &&
            (st->disposition & AV_DISPOSITION_DEFAULT))
            return (int)i;
    }
    return -1;
}

/// Picks the subtitle stream to cue (opening its decoder) and tells the
/// caller about every audio and subtitle stream. Live input gets no
/// subtitles: its clock is repaired on the fly, which cues would have to follow.
static void report_streams(remux *r) {
    tp_remux *o = r->o;
    r->sin = -1;
    if (o->cue && !o->live && o->sub_index != -2) {
        int want = o->sub_index;
        // a stream that isn't a text subtitle of this input: by language instead
        if (want >= 0 && ((unsigned)want >= r->ic->nb_streams || r->ic->streams[want]->codecpar->codec_type != AVMEDIA_TYPE_SUBTITLE ||
                          !text_subtitle(r->ic->streams[want]->codecpar->codec_id)))
            want = -1;
        if (want < 0) want = preferred_subtitle(r->ic, o->sub_lang);
        if (want >= 0 && (r->sdec = open_subtitle_decoder(r->ic->streams[want]))) {
            r->sin = want;
            // HLS starts with its subtitle renditions switched off
            r->ic->streams[want]->discard = AVDISCARD_DEFAULT;
        }
    }
    for (unsigned i = 0; i < r->ic->nb_streams; i++) {
        AVStream *st = r->ic->streams[i];
        enum AVMediaType type = st->codecpar->codec_type;
        if (type != AVMEDIA_TYPE_AUDIO && type != AVMEDIA_TYPE_SUBTITLE) continue;
        tp_stream s = {
            .index = (int)i,
            .kind = type == AVMEDIA_TYPE_AUDIO ? 1 : 2,
            .codec = avcodec_get_name(st->codecpar->codec_id),
            .lang = tag(st, "language"),
            .title = tag(st, "title"),
            .channels = type == AVMEDIA_TYPE_AUDIO ? st->codecpar->ch_layout.nb_channels : 0,
            .is_default = !!(st->disposition & AV_DISPOSITION_DEFAULT),
            .castable = type == AVMEDIA_TYPE_AUDIO || (o->cue && !o->live && text_subtitle(st->codecpar->codec_id)),
            .selected = (int)i == (type == AVMEDIA_TYPE_AUDIO ? r->ain : r->sin),
        };
        if (o->stream) o->stream(o->opaque, &s);
    }
}

/// Decodes one subtitle packet and hands its text events out as cues.
static void decode_subtitle(remux *r, AVPacket *pkt) {
    AVCodecContext *dec = r->sdec;
    AVStream *st = r->ic->streams[pkt->stream_index];
    AVSubtitle sub;
    int got = 0;
    if (avcodec_decode_subtitle2(dec, &sub, &got, pkt) < 0 || !got) return;
    int64_t pts = sub.pts;
    if (pts == AV_NOPTS_VALUE && pkt->pts != AV_NOPTS_VALUE) pts = av_rescale_q(pkt->pts, st->time_base, AV_TIME_BASE_Q);
    if (pts != AV_NOPTS_VALUE) {
        double base = pts / (double)AV_TIME_BASE;
        if (r->ic->start_time != AV_NOPTS_VALUE) base -= r->ic->start_time / (double)AV_TIME_BASE;
        double start = base + sub.start_display_time / 1000.0;
        double end;
        if (sub.end_display_time > sub.start_display_time && sub.end_display_time != UINT32_MAX)
            end = base + sub.end_display_time / 1000.0;
        else if (pkt->duration > 0)
            end = base + pkt->duration * av_q2d(st->time_base);
        else
            end = start + 5;
        for (unsigned i = 0; i < sub.num_rects; i++)
            if (sub.rects[i]->type == SUBTITLE_ASS && sub.rects[i]->ass)
                r->o->cue(r->o->opaque, pkt->stream_index, start, end, sub.rects[i]->ass);
    }
    avsubtitle_free(&sub);
}

static int open_input(remux *r) {
    tp_remux *o = r->o;
    AVDictionary *opts = NULL;
    if (o->user_agent) av_dict_set(&opts, "user_agent", o->user_agent, 0);
    if (o->referrer) av_dict_set(&opts, "referer", o->referrer, 0);
    av_dict_set(&opts, "reconnect", "1", 0);
    av_dict_set(&opts, "reconnect_streamed", "1", 0);
    av_dict_set(&opts, "reconnect_on_network_error", "1", 0);
    av_dict_set(&opts, "reconnect_delay_max", "4", 0);
    av_dict_set(&opts, "rw_timeout", "20000000", 0);
    r->ic = avformat_alloc_context();
    r->ic->interrupt_callback = (AVIOInterruptCB){interrupt_cb, o};
    int ret = avformat_open_input(&r->ic, o->url, NULL, &opts);
    av_dict_free(&opts);
    if (ret < 0) {
        fail(o, "cannot open the stream", ret);
        return ret;
    }
    if ((ret = avformat_find_stream_info(r->ic, NULL)) < 0) {
        fail(o, "cannot read the stream", ret);
        return ret;
    }

    r->vin = av_find_best_stream(r->ic, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (r->vin >= 0 && (r->ic->streams[r->vin]->disposition & AV_DISPOSITION_ATTACHED_PIC)) r->vin = -1;
    if (r->vin >= 0 && !castable_video(r->ic->streams[r->vin]->codecpar->codec_id)) {
        snprintf(o->error, sizeof o->error, "this video (%s) can't be cast",
                 avcodec_get_name(r->ic->streams[r->vin]->codecpar->codec_id));
        return AVERROR_PATCHWELCOME;
    }
    r->ain = -1;
    if (o->audio_index >= 0 && (unsigned)o->audio_index < r->ic->nb_streams &&
        r->ic->streams[o->audio_index]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO)
        r->ain = o->audio_index;
    if (r->ain < 0) r->ain = preferred_audio(r->ic, o->audio_lang);
    if (r->ain < 0) r->ain = av_find_best_stream(r->ic, AVMEDIA_TYPE_AUDIO, -1, r->vin, NULL, 0);
    if (r->ain < 0) r->ain = -1;
    if (r->vin < 0 && r->ain < 0) {
        fail(o, "the stream has no audio or video", 0);
        return AVERROR_STREAM_NOT_FOUND;
    }

    if (o->start > 0 && !o->live) {
        int64_t ts = (int64_t)(o->start * AV_TIME_BASE);
        if (r->ic->start_time != AV_NOPTS_VALUE) ts += r->ic->start_time;
        // the keyframe at or before `start`; report_start says where it is
        if (avformat_seek_file(r->ic, -1, INT64_MIN, ts, ts, 0) < 0)
            avformat_seek_file(r->ic, -1, INT64_MIN, ts, INT64_MAX, 0);
    }
    report_streams(r);
    for (unsigned i = 0; i < r->ic->nb_streams; i++)
        if ((int)i != r->vin && (int)i != r->ain && (int)i != r->sin) r->ic->streams[i]->discard = AVDISCARD_ALL;
    return 0;
}

static int open_output(remux *r) {
    tp_remux *o = r->o;
    int ret = avformat_alloc_output_context2(&r->oc, NULL, "mp4", NULL);
    if (ret < 0) {
        fail(o, "no MP4 muxer", ret);
        return ret;
    }
    const int bufsize = 64 * 1024;
    r->oc->pb = avio_alloc_context(av_malloc(bufsize), bufsize, 1, o, NULL, write_cb, NULL);
    r->oc->flags |= AVFMT_FLAG_CUSTOM_IO | AVFMT_FLAG_FLUSH_PACKETS;
    r->oc->avoid_negative_ts = AVFMT_AVOID_NEG_TS_MAKE_ZERO;

    r->vout = r->aout = -1;
    if (r->vin >= 0) {
        AVStream *in = r->ic->streams[r->vin];
        AVStream *out = avformat_new_stream(r->oc, NULL);
        avcodec_parameters_copy(out->codecpar, in->codecpar);
        out->codecpar->codec_tag = 0;
        out->time_base = in->time_base;
        r->vout = out->index;
    }
    if (r->ain >= 0) {
        AVStream *in = r->ic->streams[r->ain];
        AVStream *out = avformat_new_stream(r->oc, NULL);
        r->aout = out->index;
        if (copyable_audio(in->codecpar->codec_id)) {
            avcodec_parameters_copy(out->codecpar, in->codecpar);
            out->codecpar->codec_tag = 0;
            out->time_base = in->time_base;
            if (in->codecpar->codec_id == AV_CODEC_ID_AAC && (ret = open_adts_filter(r, in, out)) < 0) return ret;
        } else if ((ret = open_audio_transcoder(r, in, out)) < 0) {
            return ret;
        }
    }

    AVDictionary *opts = NULL;
    // fragments start at keyframes; delay_moov waits for the first packets,
    // which carry what TS input lacks in its headers (avcC, AAC config)
    av_dict_set(&opts, "movflags", "frag_keyframe+empty_moov+default_base_moof+delay_moov", 0);
    av_dict_set(&opts, "frag_duration", r->vin >= 0 ? "4000000" : "1000000", 0);
    ret = avformat_write_header(r->oc, &opts);
    av_dict_free(&opts);
    if (ret < 0) fail(o, "cannot start the MP4 stream", ret);
    return ret;
}

static void cleanup(remux *r) {
    if (r->oc) {
        if (r->oc->pb) {
            av_freep(&r->oc->pb->buffer);
            avio_context_free(&r->oc->pb);
        }
        avformat_free_context(r->oc);
    }
    avcodec_free_context(&r->sdec);
    avformat_close_input(&r->ic);
    av_bsf_free(&r->adts);
    avcodec_free_context(&r->dec);
    avcodec_free_context(&r->enc);
    swr_free(&r->swr);
    av_channel_layout_uninit(&r->swr_layout);
    if (r->fifo) av_audio_fifo_free(r->fifo);
    av_frame_free(&r->frame);
    av_frame_free(&r->eframe);
    av_packet_free(&r->pkt);
}

int tp_remux_run(tp_remux *o) {
    remux r = {.o = o, .vin = -1, .ain = -1, .sin = -1, .vout = -1, .aout = -1};
    r.last_in[0] = r.last_in[1] = AV_NOPTS_VALUE;
    r.last_out[0] = r.last_out[1] = AV_NOPTS_VALUE;
    o->error[0] = 0;
    avformat_network_init();
    r.pkt = av_packet_alloc();

    int ret = open_input(&r);
    if (ret >= 0) ret = open_output(&r);
    int header_written = ret >= 0;
    AVPacket *pkt = av_packet_alloc();
    while (ret >= 0) {
        if (o->aborted(o->opaque)) {
            ret = AVERROR_EXIT;
            break;
        }
        ret = av_read_frame(r.ic, pkt);
        if (ret == AVERROR_EOF) {
            ret = 0;
            break;
        }
        if (ret < 0) {
            fail(o, "the stream stopped", ret);
            break;
        }
        // (streams a demuxer adds later have higher indexes, never `sin`)
        if (pkt->stream_index == r.sin) {
            decode_subtitle(&r, pkt);
            av_packet_unref(pkt);
            continue;
        }
        int kind = pkt->stream_index == r.vin ? 0 : pkt->stream_index == r.ain ? 1 : -1;
        if (kind < 0) {
            av_packet_unref(pkt);
            continue;
        }
        if (o->live) repair_live(&r, pkt, kind);
        report_start(&r, pkt);
        AVStream *in = r.ic->streams[pkt->stream_index];
        if (kind == 1 && r.dec) {
            ret = transcode_audio(&r, pkt);
        } else if (kind == 1 && r.adts) {
            ret = copy_aac(&r, pkt);
        } else {
            int out = kind == 0 ? r.vout : r.aout;
            pkt->stream_index = out;
            pkt->pos = -1;
            av_packet_rescale_ts(pkt, in->time_base, r.oc->streams[out]->time_base);
            ret = mux(&r, pkt, kind);
        }
        av_packet_unref(pkt);
    }
    av_packet_free(&pkt);

    if (header_written && ret >= 0) {
        if (r.dec) {
            avcodec_send_packet(r.dec, NULL);
            transcode_audio(&r, NULL);
            drain_fifo(&r, 1);
            encode_and_mux(&r, NULL);
        }
        av_write_trailer(r.oc);
    }
    cleanup(&r);
    // stopped on purpose (also while opening: FFmpeg then reports whatever
    // the interrupted step failed with)
    if (ret == AVERROR_EXIT || o->aborted(o->opaque)) {
        o->error[0] = 0;
        return 0;
    }
    return ret < 0 ? ret : 0;
}
