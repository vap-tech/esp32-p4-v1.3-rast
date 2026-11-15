#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include "driver/ppa.h"
#include "driver/usb_serial_jtag.h"
#include "driver/usb_serial_jtag_vfs.h"
#include "esp_cache.h"
#include "esp_heap_caps.h"
#include "esp_timer.h"
#include "esp_system.h"
#include "esp_chip_info.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "soc/dma2d_struct.h"
#include "soc/ppa_struct.h"

static uint16_t bg_pixel(unsigned x,unsigned y){return (((x*3+y)&31)<<11)|(((y*5+7)&63)<<5)|((x*2+y*7)&31);}
static uint32_t fg_pixel(unsigned x,unsigned y){
 static const unsigned alpha[]={0,1,64,127,128,192,254,255};
 unsigned a=alpha[(x+2*y)%8],r=0,g=0,b=0;
 if((x+y)%17){r=(x*37+y*11)&255;g=(x*13+y*29)&255;b=(x*7+y*43)&255;}
 return (a<<24)|(r<<16)|(g<<8)|b;
}
static uint8_t *buffer(size_t n){
 unsigned caps=MALLOC_CAP_DMA|MALLOC_CAP_8BIT;
#if CONFIG_SPIRAM
 caps |= MALLOC_CAP_SPIRAM;
#else
 caps |= MALLOC_CAP_INTERNAL;
#endif
 uint8_t *b=heap_caps_aligned_alloc(64,n,caps);
 if(!b){abort();}
 memset(b,0x5a,n);return b;
}
void app_main(void){
 usb_serial_jtag_driver_config_t usb={.tx_buffer_size=2048,.rx_buffer_size=256};
 ESP_ERROR_CHECK(usb_serial_jtag_driver_install(&usb));
 usb_serial_jtag_vfs_use_driver();
 setvbuf(stdout,NULL,_IONBF,0);
 vTaskDelay(pdMS_TO_TICKS(3000));
 esp_chip_info_t chip;esp_chip_info(&chip);
 printf("RIDREF START idf=%s rev=%u cores=%u\n",esp_get_idf_version(),chip.revision,chip.cores);

 ppa_client_handle_t handle;
 ppa_client_config_t client={.oper_type=PPA_OPERATION_BLEND,.data_burst_length=PPA_DATA_BURST_LENGTH_32};
 ESP_ERROR_CHECK(ppa_register_client(&client,&handle));
 puts("RIDREF registered");
 const unsigned bw=32,bh=24,fw=35,fh=25,ow=64,oh=32;
 const size_t bg_size=bw*bh*2,fg_size=((fw*fh*4+63)/64)*64,out_size=ow*oh*2;
 uint8_t *b=buffer(bg_size+128),*f=buffer(fg_size+128),*o=buffer(out_size+128);
 puts("RIDREF allocated");
 for(unsigned y=0;y<bh;y++)for(unsigned x=0;x<bw;x++)((uint16_t*)(b+64))[y*bw+x]=bg_pixel(x,y);
 for(unsigned y=0;y<fh;y++)for(unsigned x=0;x<fw;x++)((uint32_t*)(f+64))[y*fw+x]=fg_pixel(x,y);
 puts("RIDREF initialized");
 ESP_ERROR_CHECK(esp_cache_msync(b,bg_size+128,ESP_CACHE_MSYNC_FLAG_DIR_C2M));
 ESP_ERROR_CHECK(esp_cache_msync(f,fg_size+128,ESP_CACHE_MSYNC_FLAG_DIR_C2M));
 ESP_ERROR_CHECK(esp_cache_msync(o,out_size+128,ESP_CACHE_MSYNC_FLAG_DIR_C2M));
 puts("RIDREF synced");
 for(unsigned pass=0;pass<3;pass++){
  ppa_blend_oper_config_t cfg={
   .in_bg={.buffer=b+64,.pic_w=bw,.pic_h=bh,.block_w=20,.block_h=12,.block_offset_x=4,.block_offset_y=2,.blend_cm=PPA_BLEND_COLOR_MODE_RGB565},
   .in_fg={.buffer=f+64,.pic_w=fw,.pic_h=fh,.block_w=20,.block_h=12,.block_offset_x=3,.block_offset_y=1,.blend_cm=PPA_BLEND_COLOR_MODE_ARGB8888},
   .out={.buffer=o+64,.buffer_size=out_size,.pic_w=ow,.pic_h=oh,.block_offset_x=8,.block_offset_y=5,.blend_cm=PPA_BLEND_COLOR_MODE_RGB565},
   .bg_alpha_update_mode=PPA_ALPHA_FIX_VALUE,.bg_alpha_fix_val=255,
   .fg_alpha_update_mode=PPA_ALPHA_NO_CHANGE,.mode=PPA_TRANS_MODE_BLOCKING,
  };
  puts("RIDREF entering blend");
  uint32_t before=DMA2D.axi_err.val;
  int64_t t=esp_timer_get_time();esp_err_t ret=ppa_do_blend(handle,&cfg);t=esp_timer_get_time()-t;
  uint32_t after=DMA2D.axi_err.val;
  ESP_ERROR_CHECK(esp_cache_msync(o,out_size+128,ESP_CACHE_MSYNC_FLAG_DIR_M2C));
  unsigned bad=0,maxerr=0,guards=0;
  for(unsigned i=0;i<64;i++)if(b[i]!=0x5a||b[bg_size+64+i]!=0x5a||f[i]!=0x5a||f[fg_size+64+i]!=0x5a||o[i]!=0x5a||o[out_size+64+i]!=0x5a)guards++;
  for(unsigned y=0;y<oh;y++)for(unsigned x=0;x<ow;x++){
   unsigned expected=0x5a5a,a=0;int inside=x>=8&&x<28&&y>=5&&y<17;
   if(inside){
    unsigned bv=bg_pixel(x-8+4,y-5+2),fv=fg_pixel(x-8+3,y-5+1);a=fv>>24;
    unsigned r=bv>>11,g=(bv>>5)&63,bl=bv&31;
    r=(r<<3)|(r>>2);g=(g<<2)|(g>>4);bl=(bl<<3)|(bl>>2);
    r=(r*(255-a)+((fv>>16)&255)*a+127)/255;
    g=(g*(255-a)+((fv>>8)&255)*a+127)/255;
    bl=(bl*(255-a)+(fv&255)*a+127)/255;
    expected=((r>>3)<<11)|((g>>2)<<5)|(bl>>3);
   }
   unsigned actual=((uint16_t*)(o+64))[y*ow+x];
   unsigned er=abs((int)(actual>>11)-(int)(expected>>11)),eg=abs((int)((actual>>5)&63)-(int)((expected>>5)&63)),eb=abs((int)(actual&31)-(int)(expected&31));
   if(er>maxerr){maxerr=er;}
   if(eg>maxerr){maxerr=eg;}
   if(eb>maxerr){maxerr=eb;}
   if(((!inside||a==0||a==255)&&actual!=expected)||er>1||eg>1||eb>1)bad++;
  }
  printf("RIDREF PASS=%u ret=%s us=%lld before=%08lx after=%08lx bad=%u max=%u guards=%u tx0=%08lx tx1=%08lx rx0=%08lx rx1=%08lx\n",pass,esp_err_to_name(ret),(long long)t,(unsigned long)before,(unsigned long)after,bad,maxerr,guards,(unsigned long)DMA2D.out_channel[0].out_conf0.val,(unsigned long)DMA2D.out_channel[1].out_conf0.val,(unsigned long)DMA2D.in_channel[0].in_conf0.val,(unsigned long)DMA2D.in_channel[1].in_conf0.val);
 }
 printf("RIDREF DONE axi=%08lx\n",(unsigned long)DMA2D.axi_err.val);

 // Keep the module claimed: do not gate off diagnostics before their capture.
 for(;;)vTaskDelay(pdMS_TO_TICKS(1000));
}
