// Benchmark 5: N-body simulation (C baseline)
#include <stdio.h>
#include <math.h>
int main() {
    double sun_x=0,sun_y=0,sun_z=0,sun_vx=0,sun_vy=0,sun_vz=0,sun_m=1.0;
    double jup_x=4.84143144246472090,jup_y=-1.16032004402742839,jup_z=-0.10327910045777416;
    double jup_vx=0.00166003245*365.25,jup_vy=0.007699011275*365.25,jup_vz=-0.0000690460016*365.25,jup_m=0.000954791938;
    double sat_x=8.34336671824457987,sat_y=4.12479856412430479,sat_z=-0.40352044297;
    double sat_vx=-0.002767425107*365.25,sat_vy=0.004998528012*365.25,sat_vz=0.0000230417297*365.25,sat_m=0.000285885980;
    double dt=0.01;
    for (int i = 0; i < 500000; i++) {
        double dx,dy,dz,d2,d,mag;
        // Sun-Jupiter
        dx=sun_x-jup_x; dy=sun_y-jup_y; dz=sun_z-jup_z;
        d2=dx*dx+dy*dy+dz*dz; d=sqrt(d2); mag=dt/(d2*d);
        sun_vx-=dx*jup_m*mag; sun_vy-=dy*jup_m*mag; sun_vz-=dz*jup_m*mag;
        jup_vx+=dx*sun_m*mag; jup_vy+=dy*sun_m*mag; jup_vz+=dz*sun_m*mag;
        // Sun-Saturn
        dx=sun_x-sat_x; dy=sun_y-sat_y; dz=sun_z-sat_z;
        d2=dx*dx+dy*dy+dz*dz; d=sqrt(d2); mag=dt/(d2*d);
        sun_vx-=dx*sat_m*mag; sun_vy-=dy*sat_m*mag; sun_vz-=dz*sat_m*mag;
        sat_vx+=dx*sun_m*mag; sat_vy+=dy*sun_m*mag; sat_vz+=dz*sun_m*mag;
        // Jupiter-Saturn
        dx=jup_x-sat_x; dy=jup_y-sat_y; dz=jup_z-sat_z;
        d2=dx*dx+dy*dy+dz*dz; d=sqrt(d2); mag=dt/(d2*d);
        jup_vx-=dx*sat_m*mag; jup_vy-=dy*sat_m*mag; jup_vz-=dz*sat_m*mag;
        sat_vx+=dx*jup_m*mag; sat_vy+=dy*jup_m*mag; sat_vz+=dz*jup_m*mag;
        // Positions
        sun_x+=dt*sun_vx; sun_y+=dt*sun_vy; sun_z+=dt*sun_vz;
        jup_x+=dt*jup_vx; jup_y+=dt*jup_vy; jup_z+=dt*jup_vz;
        sat_x+=dt*sat_vx; sat_y+=dt*sat_vy; sat_z+=dt*sat_vz;
    }
    double ke = sun_m*(sun_vx*sun_vx+sun_vy*sun_vy+sun_vz*sun_vz)*0.5
              + jup_m*(jup_vx*jup_vx+jup_vy*jup_vy+jup_vz*jup_vz)*0.5
              + sat_m*(sat_vx*sat_vx+sat_vy*sat_vy+sat_vz*sat_vz)*0.5;
    printf("nbody 500k steps, KE = %g\n", ke);
    return 0;
}
